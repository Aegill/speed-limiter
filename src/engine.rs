//! Движок: перехват входящих пакетов через WinDivert и их задержка по лимитам приложений.
//!
//! WinDivert подгружается во время выполнения (`WinDivert.dll` рядом с программой), поэтому
//! сам код собирается без внешних библиотек. Нужны права администратора.

use std::collections::HashMap;
use std::ffi::{CString, c_char};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libloading::Library;

use crate::model::{AppState, Shared};
use crate::packet::parse_inbound;
use crate::shaper::{Shaper, Verdict};

// ---------------------------------------------------------------------------------------------
// Привязки к WinDivert 2.2
// ---------------------------------------------------------------------------------------------

const LAYER_NETWORK: i32 = 0;
const LAYER_FLOW: i32 = 2;
const EVENT_FLOW_ESTABLISHED: u32 = 1;
const EVENT_FLOW_DELETED: u32 = 2;
const FLAG_SNIFF: u64 = 1;
const FLAG_RECV_ONLY: u64 = 4;
const SHUTDOWN_BOTH: i32 = 3;
const PARAM_QUEUE_LENGTH: i32 = 0;
const PARAM_QUEUE_TIME: i32 = 1;
const PARAM_QUEUE_SIZE: i32 = 2;
const INVALID_HANDLE: isize = -1;
const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

#[repr(C)]
#[derive(Clone, Copy)]
struct FlowData {
    endpoint_id: u64,
    parent_endpoint_id: u64,
    process_id: u32,
    local_addr: [u32; 4],
    remote_addr: [u32; 4],
    local_port: u16,
    remote_port: u16,
    protocol: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
union AddrData {
    flow: FlowData,
    raw: [u8; 64],
}

/// WINDIVERT_ADDRESS (80 байт). `flags`: Layer:8, Event:8, Sniffed, Outbound, Loopback, ...
#[repr(C)]
#[derive(Clone, Copy)]
struct Address {
    timestamp: i64,
    flags: u32,
    reserved2: u32,
    data: AddrData,
}

const _: () = assert!(std::mem::size_of::<Address>() == 80);

impl Address {
    fn zeroed() -> Self {
        // Допустимо: структура состоит только из целых чисел.
        unsafe { std::mem::zeroed() }
    }
    fn event(&self) -> u32 {
        (self.flags >> 8) & 0xff
    }
}

type OpenFn = unsafe extern "system" fn(*const c_char, i32, i16, u64) -> isize;
type RecvFn = unsafe extern "system" fn(isize, *mut u8, u32, *mut u32, *mut Address) -> i32;
type SendFn = unsafe extern "system" fn(isize, *const u8, u32, *mut u32, *const Address) -> i32;
type CloseFn = unsafe extern "system" fn(isize) -> i32;
type ShutdownFn = unsafe extern "system" fn(isize, i32) -> i32;
type SetParamFn = unsafe extern "system" fn(isize, i32, u64) -> i32;

struct Api {
    open: OpenFn,
    recv: RecvFn,
    send: SendFn,
    close: CloseFn,
    shutdown: ShutdownFn,
    set_param: SetParamFn,
    _lib: Library, // держим библиотеку загруженной, пока живут указатели
}

impl Api {
    fn load() -> Result<Self, String> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = std::env::current_exe().ok().as_deref().and_then(|e| e.parent()) {
            candidates.push(dir.join("WinDivert.dll"));
        }
        candidates.push(PathBuf::from("WinDivert.dll"));

        let mut last_err = String::new();
        for path in candidates {
            match unsafe { Library::new(&path) } {
                Ok(lib) => return unsafe { Self::bind(lib) },
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(format!(
            "Не удалось загрузить WinDivert.dll ({last_err}). Положите WinDivert.dll и \
             WinDivert64.sys рядом с программой (скрипт get-windivert.ps1 сделает это сам)."
        ))
    }

    unsafe fn bind(lib: Library) -> Result<Self, String> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *lib.get::<$ty>($name).map_err(|e| format!("в WinDivert.dll нет функции: {e}"))?
            };
        }
        unsafe {
            Ok(Self {
                open: sym!(b"WinDivertOpen\0", OpenFn),
                recv: sym!(b"WinDivertRecv\0", RecvFn),
                send: sym!(b"WinDivertSend\0", SendFn),
                close: sym!(b"WinDivertClose\0", CloseFn),
                shutdown: sym!(b"WinDivertShutdown\0", ShutdownFn),
                set_param: sym!(b"WinDivertSetParam\0", SetParamFn),
                _lib: lib,
            })
        }
    }

    fn open_handle(&self, filter: &str, layer: i32, flags: u64) -> Result<isize, String> {
        let filter = CString::new(filter).unwrap();
        let h = unsafe { (self.open)(filter.as_ptr(), layer, 0, flags) };
        if h == INVALID_HANDLE || h == 0 {
            let err = std::io::Error::last_os_error();
            return Err(explain_open_error(err.raw_os_error().unwrap_or(0), &err));
        }
        Ok(h)
    }
}

fn explain_open_error(code: i32, err: &std::io::Error) -> String {
    match code {
        5 => "Нет доступа: запустите программу от имени администратора.".into(),
        2 | 3 => "Не найден драйвер WinDivert64.sys: он должен лежать в одной папке с \
                  WinDivert.dll."
            .into(),
        577 => "Windows не приняла подпись драйвера WinDivert. Нужна официальная версия 2.2+.".into(),
        1275 => "Драйвер WinDivert заблокирован политикой Windows (антивирус, Smart App Control \
                 или список заблокированных драйверов)."
            .into(),
        1060 => "Служба драйвера WinDivert не установлена.".into(),
        _ => format!("Не удалось открыть WinDivert: {err} (код {code})."),
    }
}

// ---------------------------------------------------------------------------------------------
// Точность таймера Windows (по умолчанию ~15 мс, а нам нужна ~1 мс)
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod timer {
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn timeBeginPeriod(period: u32) -> u32;
        fn timeEndPeriod(period: u32) -> u32;
    }
    pub fn begin() {
        unsafe { timeBeginPeriod(1) };
    }
    pub fn end() {
        unsafe { timeEndPeriod(1) };
    }
}
#[cfg(not(windows))]
mod timer {
    pub fn begin() {}
    pub fn end() {}
}

// ---------------------------------------------------------------------------------------------
// Захват и шейпинг
// ---------------------------------------------------------------------------------------------

struct Held {
    data: Vec<u8>,
    addr: Address,
}

struct Entry {
    app: Arc<AppState>,
    shaper: Shaper<Held>,
}

type Shapers = Arc<Mutex<HashMap<usize, Entry>>>;

pub struct Running {
    api: Arc<Api>,
    h_net: isize,
    h_flow: Option<isize>,
    stop: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<String>>>,
    threads: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
}

fn send_packet(api: &Api, h: isize, held: &Held) {
    unsafe {
        (api.send)(
            h,
            held.data.as_ptr(),
            held.data.len() as u32,
            std::ptr::null_mut(),
            &held.addr,
        );
    }
}

impl Running {
    fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let api = Arc::new(Api::load()?);

        let h_net = api.open_handle(
            "inbound and !loopback and (tcp or udp)",
            LAYER_NETWORK,
            0,
        )?;
        unsafe {
            (api.set_param)(h_net, PARAM_QUEUE_LENGTH, 16384);
            (api.set_param)(h_net, PARAM_QUEUE_TIME, 1000);
            (api.set_param)(h_net, PARAM_QUEUE_SIZE, 32 * 1024 * 1024);
        }
        // Слой потоков нужен только для мгновенного определения владельца соединения.
        // Если он недоступен — обходимся периодическим опросом таблицы сокетов.
        let h_flow = api
            .open_handle("true", LAYER_FLOW, FLAG_SNIFF | FLAG_RECV_ONLY)
            .ok();

        timer::begin();

        let stop = Arc::new(AtomicBool::new(false));
        let fault = Arc::new(Mutex::new(None));
        let shapers: Shapers = Arc::new(Mutex::new(HashMap::new()));
        let mut threads = Vec::new();

        {
            let (api, shared, shapers, stop, fault) = (
                api.clone(),
                shared.clone(),
                shapers.clone(),
                stop.clone(),
                fault.clone(),
            );
            threads.push(
                std::thread::Builder::new()
                    .name("net-recv".into())
                    .spawn(move || net_loop(&api, h_net, &shared, &shapers, &stop, &fault))
                    .map_err(|e| e.to_string())?,
            );
        }
        {
            let (api, shapers, stop) = (api.clone(), shapers.clone(), stop.clone());
            threads.push(
                std::thread::Builder::new()
                    .name("net-release".into())
                    .spawn(move || release_loop(&api, h_net, &shapers, &stop))
                    .map_err(|e| e.to_string())?,
            );
        }
        if let Some(h) = h_flow {
            let (api, shared, stop) = (api.clone(), shared.clone(), stop.clone());
            threads.push(
                std::thread::Builder::new()
                    .name("flow-recv".into())
                    .spawn(move || flow_loop(&api, h, &shared, &stop))
                    .map_err(|e| e.to_string())?,
            );
        }

        Ok(Self { api, h_net, h_flow, stop, fault, threads, shared })
    }

    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            (self.api.shutdown)(self.h_net, SHUTDOWN_BOTH);
            if let Some(h) = self.h_flow {
                (self.api.shutdown)(h, SHUTDOWN_BOTH);
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        unsafe {
            (self.api.close)(self.h_net);
            if let Some(h) = self.h_flow {
                (self.api.close)(h);
            }
        }
        timer::end();
        self.shared.clear_flows();
    }
}

fn net_loop(
    api: &Api,
    h: isize,
    shared: &Shared,
    shapers: &Shapers,
    stop: &AtomicBool,
    fault: &Mutex<Option<String>>,
) {
    let mut buf = vec![0u8; 65_535 + 64];
    let mut errors = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let mut addr = Address::zeroed();
        let mut n = 0u32;
        let ok = unsafe { (api.recv)(h, buf.as_mut_ptr(), buf.len() as u32, &mut n, &mut addr) };
        if ok == 0 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER) {
                continue;
            }
            errors += 1;
            if errors > 200 {
                *fault.lock().unwrap() = Some(format!("Сбой чтения пакетов WinDivert: {err}"));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        errors = 0;
        let n = n as usize;
        let packet = &buf[..n];

        let app = parse_inbound(packet)
            .and_then(|t| shared.lookup_port(t.proto, t.local_port));
        let Some(app) = app else {
            send_packet(api, h, &Held { data: packet.to_vec(), addr });
            continue;
        };
        app.bytes_in.fetch_add(n as u64, Ordering::Relaxed);

        let limit = app.limit_bps.load(Ordering::Relaxed);
        if limit == 0 {
            // Без копирования: отправляем тот же буфер.
            unsafe {
                (api.send)(h, packet.as_ptr(), n as u32, std::ptr::null_mut(), &addr);
            }
            continue;
        }

        let held = Held { data: packet.to_vec(), addr };
        let now = Instant::now();
        let verdict = {
            let mut map = shapers.lock().unwrap();
            let id = Arc::as_ptr(&app) as usize;
            let entry = map.entry(id).or_insert_with(|| Entry {
                app: app.clone(),
                shaper: Shaper::new(limit, now),
            });
            if entry.shaper.rate() != limit {
                entry.shaper.set_rate(limit);
            }
            entry.shaper.push(now, held, n)
        };
        match verdict {
            Verdict::Pass(held) => send_packet(api, h, &held),
            Verdict::Queued => {}
            Verdict::Dropped => {
                app.dropped.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
}

fn release_loop(api: &Api, h: isize, shapers: &Shapers, stop: &AtomicBool) {
    let mut out: Vec<Held> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
        let now = Instant::now();
        {
            let mut map = shapers.lock().unwrap();
            map.retain(|_, e| {
                let limit = e.app.limit_bps.load(Ordering::Relaxed);
                if limit == 0 {
                    // лимит сняли — отдаём всё накопленное и забываем шейпер
                    e.shaper.drain(&mut out);
                    return false;
                }
                if e.shaper.rate() != limit {
                    e.shaper.set_rate(limit);
                }
                if !e.shaper.is_idle() {
                    e.shaper.poll(now, &mut out);
                }
                true
            });
        }
        for held in out.drain(..) {
            send_packet(api, h, &held);
        }
    }
}

fn flow_loop(api: &Api, h: isize, shared: &Shared, stop: &AtomicBool) {
    let mut errors = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let mut addr = Address::zeroed();
        let ok = unsafe {
            (api.recv)(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut addr)
        };
        if ok == 0 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            errors += 1;
            if errors > 200 {
                break; // не критично: останется опрос таблицы сокетов
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        errors = 0;
        let flow = unsafe { addr.data.flow };
        match addr.event() {
            EVENT_FLOW_ESTABLISHED => {
                if flow.local_port != 0
                    && let Some(app) = shared.resolve_pid(flow.process_id)
                {
                    shared.flow_open(flow.protocol, flow.local_port, app);
                }
            }
            EVENT_FLOW_DELETED => shared.flow_close(flow.protocol, flow.local_port),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Публичный интерфейс для UI
// ---------------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Off,
    On,
    Failed(String),
}

pub struct Engine {
    shared: Arc<Shared>,
    running: Option<Running>,
    pub status: Status,
}

impl Engine {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared, running: None, status: Status::Off }
    }

    pub fn set_enabled(&mut self, on: bool) {
        if on {
            if !cfg!(windows) {
                self.status =
                    Status::Failed("Перехват трафика поддерживается только в Windows.".into());
                return;
            }
            if self.running.is_none() {
                match Running::start(self.shared.clone()) {
                    Ok(r) => {
                        self.running = Some(r);
                        self.status = Status::On;
                    }
                    Err(e) => self.status = Status::Failed(e),
                }
            }
        } else {
            if let Some(r) = self.running.take() {
                r.stop();
            }
            self.status = Status::Off;
        }
    }

    /// Вызывать периодически: замечает аварийную остановку фоновых потоков.
    pub fn tick(&mut self) {
        let fault = self.running.as_ref().and_then(|r| r.fault());
        if let Some(msg) = fault {
            if let Some(r) = self.running.take() {
                r.stop();
            }
            self.status = Status::Failed(msg);
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.set_enabled(false);
    }
}

// ---------------------------------------------------------------------------------------------
// Интеграционный тест на Linux: вместо настоящего WinDivert подставляется «заглушка» на C,
// которая генерирует входящий поток ~9 МБ/с и считает, сколько байт было отправлено обратно.
// ---------------------------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod mock_tests {
    use super::*;

    const MOCK_C: &str = r#"
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
static volatile int shut = 0;
static volatile long sent = 0;
typedef struct { int64_t ts; uint32_t flags; uint32_t r2; uint8_t data[64]; } ADDR;
intptr_t WinDivertOpen(const char* f, int layer, int16_t pr, uint64_t fl) { return layer == 0 ? 1 : 2; }
int WinDivertRecv(intptr_t h, uint8_t* pkt, uint32_t len, uint32_t* rl, ADDR* a) {
    if (h == 2) { while (!shut) usleep(1000); errno = 232; return 0; }
    if (shut) { errno = 232; return 0; }
    usleep(100);
    memset(pkt, 0, 1500);
    pkt[0] = 0x45; pkt[9] = 6; pkt[22] = 0x15; pkt[23] = 0xB3; /* порт 5555 */
    *rl = 1500; memset(a, 0, sizeof *a);
    return 1;
}
int WinDivertSend(intptr_t h, const uint8_t* p, uint32_t len, uint32_t* sl, const ADDR* a) { sent += len; return 1; }
int WinDivertClose(intptr_t h) { return 1; }
int WinDivertShutdown(intptr_t h, int how) { shut = 1; return 1; }
int WinDivertSetParam(intptr_t h, int p, uint64_t v) { return 1; }
long mock_sent(void) { return sent; }
"#;

    #[test]
    fn limits_throughput_end_to_end() {
        let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let src = dir.join("windivert_mock.c");
        let lib = dir.join("WinDivert.dll");
        std::fs::write(&src, MOCK_C).unwrap();
        let ok = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-O1", "-o"])
            .arg(&lib)
            .arg(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("cc недоступен — тест пропущен");
            return;
        }

        let shared = Shared::new();
        let app = shared.app("test.exe");
        shared.flow_open(6, 5555, app.clone());
        app.limit_bps.store(200_000, Ordering::Relaxed);

        let running = Running::start(shared.clone()).expect("start");
        let probe = unsafe { Library::new(&lib) }.unwrap();
        let sent = || unsafe { (probe.get::<unsafe extern "C" fn() -> i64>(b"mock_sent\0").unwrap())() };

        std::thread::sleep(Duration::from_secs(3));
        let limited = sent();
        // снимаем лимит: поток должен пойти без ограничений
        app.limit_bps.store(0, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
        let free = sent() - limited;
        let received = app.bytes_in.load(Ordering::Relaxed);
        running.stop();

        eprintln!("с лимитом 200 КБ/с за 3 с отправлено {limited} Б; без лимита за 1 с — {free} Б; принято {received} Б");
        assert!(
            (500_000..=700_000).contains(&limited),
            "ожидали ~600 КБ за 3 с, получили {limited}"
        );
        assert!(free > 2_000_000, "после снятия лимита ожидали >2 МБ/с, получили {free}");
        assert!(app.dropped.load(Ordering::Relaxed) > 0, "при перегрузке пакеты должны отбрасываться");
    }
}
