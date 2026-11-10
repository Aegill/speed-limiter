//! Шейпер на основе token bucket с очередью пакетов.
//!
//! Модуль не зависит от ОС и от источника пакетов: время передаётся снаружи,
//! поэтому логику легко проверять тестами.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Что сделать с пакетом сразу после постановки в шейпер.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict<P> {
    /// Пакет можно отправить немедленно.
    Pass(P),
    /// Пакет поставлен в очередь, его вернёт `poll`.
    Queued,
    /// Очередь переполнена, пакет отброшен (TCP это заметит и снизит скорость).
    Dropped,
}

pub struct Shaper<P> {
    rate: f64,   // байт/с
    tokens: f64, // может уходить в минус: большой пакет «берётся в долг»
    last: Instant,
    queue: VecDeque<(P, usize)>,
    queued_bytes: usize,
}

impl<P> Shaper<P> {
    pub fn new(rate_bps: u64, now: Instant) -> Self {
        let rate = rate_bps.max(1) as f64;
        Self {
            rate,
            tokens: Self::burst_for(rate),
            last: now,
            queue: VecDeque::new(),
            queued_bytes: 0,
        }
    }

    /// Максимальный «запас» токенов: 50 мс трафика, но не меньше пары пакетов.
    fn burst_for(rate: f64) -> f64 {
        (rate * 0.05).max(4000.0)
    }

    /// Максимальный размер очереди: ~250 мс трафика, но не меньше нескольких пакетов.
    fn queue_limit_for(rate: f64) -> usize {
        (rate * 0.25).max(6000.0) as usize
    }

    pub fn rate(&self) -> u64 {
        self.rate as u64
    }

    pub fn set_rate(&mut self, rate_bps: u64) {
        self.rate = rate_bps.max(1) as f64;
        self.tokens = self.tokens.min(Self::burst_for(self.rate));
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_empty()
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(Self::burst_for(self.rate));
    }

    pub fn push(&mut self, now: Instant, packet: P, len: usize) -> Verdict<P> {
        self.refill(now);
        if self.queue.is_empty() && self.tokens > 0.0 {
            self.tokens -= len as f64;
            return Verdict::Pass(packet);
        }
        if self.queued_bytes + len > Self::queue_limit_for(self.rate) {
            return Verdict::Dropped;
        }
        self.queued_bytes += len;
        self.queue.push_back((packet, len));
        Verdict::Queued
    }

    /// Достаёт из очереди пакеты, которые уже можно отправить.
    pub fn poll(&mut self, now: Instant, out: &mut Vec<P>) {
        self.refill(now);
        while self.tokens > 0.0 {
            let Some((packet, len)) = self.queue.pop_front() else {
                break;
            };
            self.queued_bytes -= len;
            self.tokens -= len as f64;
            out.push(packet);
        }
    }

    /// Забирает всё, что осталось в очереди (при остановке или снятии лимита).
    pub fn drain(&mut self, out: &mut Vec<P>) {
        out.extend(self.queue.drain(..).map(|(p, _)| p));
        self.queued_bytes = 0;
    }
}

/// Удобный помощник для тестов и отладки.
#[allow(dead_code)]
pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Прогоняет `secs` секунд: каждую миллисекунду приходит `pkts_per_ms` пакетов
    /// по 1500 байт, шейпер опрашивается раз в миллисекунду.
    fn simulate(rate: u64, secs: u64, pkts_per_ms: usize) -> (usize, usize, usize) {
        let t0 = Instant::now();
        let mut s = Shaper::new(rate, t0);
        let (mut passed, mut dropped) = (0usize, 0usize);
        let mut out = Vec::new();
        for step in 1..=(secs * 1000) {
            let now = t0 + ms(step);
            for _ in 0..pkts_per_ms {
                match s.push(now, (), 1500) {
                    Verdict::Pass(_) => passed += 1500,
                    Verdict::Queued => {}
                    Verdict::Dropped => dropped += 1500,
                }
            }
            out.clear();
            s.poll(now, &mut out);
            passed += out.len() * 1500;
        }
        (passed, dropped, s.queued_bytes)
    }

    #[test]
    fn rate_is_respected() {
        for rate in [50_000u64, 500_000, 5_000_000] {
            let (passed, dropped, _) = simulate(rate, 10, 20);
            let expected = rate as usize * 10;
            let err = (passed as f64 - expected as f64).abs() / expected as f64;
            assert!(err < 0.03, "rate {rate}: passed {passed}, expected ~{expected}");
            assert!(dropped > 0, "при перегрузке должны быть отбрасывания");
        }
    }

    #[test]
    fn under_limit_is_untouched() {
        // 1 пакет каждые 10 мс = 150 КБ/с при лимите 1 МБ/с
        let t0 = Instant::now();
        let mut s = Shaper::new(1_000_000, t0);
        for i in 1..=500u64 {
            let v = s.push(t0 + ms(i * 10), i, 1500);
            assert_eq!(v, Verdict::Pass(i));
        }
    }

    #[test]
    fn queue_is_bounded() {
        let (_, _, queued) = simulate(100_000, 5, 50);
        assert!(queued <= 100_000 / 4 + 1500, "очередь выросла до {queued}");
    }

    #[test]
    fn big_packet_does_not_deadlock() {
        // пакет больше «запаса» токенов не должен застрять навсегда
        let t0 = Instant::now();
        let mut s = Shaper::new(10_000, t0);
        assert!(matches!(s.push(t0, 1, 60_000), Verdict::Pass(1)));
        assert!(matches!(s.push(t0, 2, 1500), Verdict::Queued));
        let mut out = Vec::new();
        s.poll(t0 + ms(3000), &mut out); // долг 60 КБ гасится за 6 секунд
        assert!(out.is_empty());
        s.poll(t0 + ms(7000), &mut out);
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn drain_returns_everything() {
        let t0 = Instant::now();
        let mut s = Shaper::new(1000, t0);
        // запас токенов 4000 Б: первые три пакета проходят сразу, остальные ждут в очереди
        for i in 1..=5 {
            let _ = s.push(t0, i, 1500);
        }
        let mut out = Vec::new();
        s.drain(&mut out);
        assert_eq!(out, vec![4, 5]);
        assert!(s.is_idle());
    }
}
