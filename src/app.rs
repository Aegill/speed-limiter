//! Главное окно приложения.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText,
    Sense, Stroke, StrokeKind, TextEdit, vec2,
};

use crate::config::{Config, RuleCfg, Unit, limit_bps, parse_value};
use crate::engine::{Engine, Status};
use crate::model::{AppState, Shared};
use crate::theme::{self, Palette};

/// Настройки одного приложения, как их видит интерфейс.
struct Rule {
    name: String,
    enabled: bool,
    /// Текст в поле ввода (может быть временно некорректным).
    text: String,
    /// Последнее корректное значение.
    value: f64,
    unit: Unit,
}

impl Rule {
    fn new(name: &str) -> Self {
        Self { name: name.to_string(), enabled: false, text: "1".into(), value: 1.0, unit: Unit::MBps }
    }

    fn from_cfg(name: &str, c: &RuleCfg) -> Self {
        Self {
            name: name.to_string(),
            enabled: c.enabled,
            text: fmt_number(c.value),
            value: c.value,
            unit: c.unit,
        }
    }

    fn is_default(&self) -> bool {
        !self.enabled && self.value == 1.0 && self.unit == Unit::MBps
    }

    fn text_valid(&self) -> bool {
        parse_value(&self.text).is_some()
    }

    /// Лимит в байтах/с, который сейчас действует (0 — нет ограничения).
    fn effective_bps(&self) -> u64 {
        if !self.enabled {
            return 0;
        }
        parse_value(&self.text)
            .and_then(|v| limit_bps(v, self.unit))
            .unwrap_or(0)
    }
}

struct Stat {
    prev: u64,
    speed: f64,
    last_active: Instant,
}

pub struct App {
    shared: Arc<Shared>,
    engine: Engine,
    cfg: Config,
    pal: Palette,
    rules: HashMap<String, Rule>,
    stats: HashMap<usize, Stat>,
    last_stats: Instant,
    order: Vec<Arc<AppState>>,
    last_order: Instant,
    order_dirty: bool,
    search: String,
    dirty_since: Option<Instant>,
}

fn id_of(app: &Arc<AppState>) -> usize {
    Arc::as_ptr(app) as usize
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shared: Arc<Shared>) -> Self {
        let cfg = Config::load();
        let pal = if cfg.dark { Palette::dark() } else { Palette::light() };
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &pal);

        let mut rules = HashMap::new();
        for (name, c) in &cfg.rules {
            let rule = Rule::from_cfg(name, c);
            shared
                .app(name)
                .limit_bps
                .store(rule.effective_bps(), Ordering::Relaxed);
            rules.insert(name.to_lowercase(), rule);
        }

        let mut engine = Engine::new(shared.clone());
        engine.set_enabled(cfg.master);

        Self {
            shared,
            engine,
            cfg,
            pal,
            rules,
            stats: HashMap::new(),
            last_stats: Instant::now(),
            order: Vec::new(),
            last_order: Instant::now() - Duration::from_secs(10),
            order_dirty: true,
            search: String::new(),
            dirty_since: None,
        }
    }

    // ----- данные ---------------------------------------------------------------------------

    fn apply_rule(&self, key: &str) {
        if let Some(r) = self.rules.get(key) {
            self.shared
                .app(&r.name)
                .limit_bps
                .store(r.effective_bps(), Ordering::Relaxed);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty_since = Some(Instant::now());
        self.order_dirty = true;
    }

    fn persist(&mut self) {
        self.cfg.rules = self
            .rules
            .values()
            .filter(|r| r.enabled || !r.is_default())
            .map(|r| {
                (
                    r.name.clone(),
                    RuleCfg { enabled: r.enabled, value: r.value, unit: r.unit },
                )
            })
            .collect();
        self.cfg.save();
        self.dirty_since = None;
    }

    fn update_stats(&mut self) {
        let dt = self.last_stats.elapsed();
        if dt < Duration::from_millis(250) {
            return;
        }
        self.last_stats = Instant::now();
        let secs = dt.as_secs_f64();
        let now = Instant::now();
        for app in self.shared.apps_snapshot() {
            let cur = app.bytes_in.load(Ordering::Relaxed);
            let st = self.stats.entry(id_of(&app)).or_insert(Stat {
                prev: cur,
                speed: 0.0,
                last_active: now - Duration::from_secs(3600),
            });
            let inst = cur.saturating_sub(st.prev) as f64 / secs;
            st.prev = cur;
            st.speed = st.speed * 0.6 + inst * 0.4;
            if st.speed < 1.0 {
                st.speed = 0.0;
            }
            if inst > 0.0 {
                st.last_active = now;
            }
        }
    }

    fn speed_of(&self, app: &Arc<AppState>) -> f64 {
        self.stats.get(&id_of(app)).map_or(0.0, |s| s.speed)
    }

    fn rebuild_order(&mut self) {
        let q = self.search.trim().to_lowercase();
        let now = Instant::now();
        let mut list: Vec<Arc<AppState>> = self
            .shared
            .apps_snapshot()
            .into_iter()
            .filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q))
            .filter(|a| {
                self.cfg.show_all
                    || self.rules.get(&a.name.to_lowercase()).is_some_and(|r| r.enabled)
                    || a.conns.load(Ordering::Relaxed) > 0
                    || self
                        .stats
                        .get(&id_of(a))
                        .is_some_and(|s| now.duration_since(s.last_active) < Duration::from_secs(30))
            })
            .collect();
        list.sort_by(|a, b| {
            let limited = |x: &Arc<AppState>| {
                self.rules.get(&x.name.to_lowercase()).is_some_and(|r| r.enabled)
            };
            limited(b)
                .cmp(&limited(a))
                .then_with(|| {
                    if self.cfg.sort_by_speed {
                        self.speed_of(b).total_cmp(&self.speed_of(a))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.order = list;
        self.last_order = now;
        self.order_dirty = false;
    }

    // ----- части интерфейса -----------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new("Лимит скорости").heading());
                let (text, color) = match &self.engine.status {
                    Status::On => ("Ограничения работают", p.primary),
                    Status::Off => ("Выключено", p.on_variant),
                    Status::Failed(_) => ("Не удалось запустить", p.error),
                };
                ui.label(RichText::new(text).small().color(color));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut on = self.cfg.master;
                if switch(ui, &mut on, &p).changed() {
                    self.cfg.master = on;
                    self.engine.set_enabled(on);
                    self.mark_dirty();
                }
                ui.label("Ограничения");
                ui.add_space(16.0);
                let label = if self.cfg.dark { "Светлая тема" } else { "Тёмная тема" };
                let btn = Button::new(RichText::new(label).color(p.primary))
                    .frame(false)
                    .min_size(vec2(0.0, 32.0));
                if ui.add(btn).clicked() {
                    self.cfg.dark = !self.cfg.dark;
                    self.pal = if self.cfg.dark { Palette::dark() } else { Palette::light() };
                    theme::apply(ui.ctx(), &self.pal);
                    self.mark_dirty();
                }
            });
        });
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let Status::Failed(msg) = self.engine.status.clone() else { return };
        Frame::new()
            .fill(p.error_container)
            .corner_radius(16)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&msg).color(p.on_error_container)).wrap(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let retry = Button::new(RichText::new("Повторить").color(p.on_primary))
                            .fill(p.primary)
                            .stroke(Stroke::NONE);
                        if ui.add(retry).clicked() {
                            self.engine.set_enabled(true);
                        }
                    });
                });
            });
        ui.add_space(8.0);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        ui.horizontal(|ui| {
            ui.scope(|ui| {
                let v = ui.visuals_mut();
                v.extreme_bg_color = p.container_high;
                v.widgets.inactive.bg_stroke = Stroke::NONE;
                v.widgets.hovered.bg_stroke = Stroke::NONE;
                v.widgets.inactive.corner_radius = CornerRadius::same(28);
                v.widgets.hovered.corner_radius = CornerRadius::same(28);
                let w = (ui.available_width() - 300.0).max(160.0);
                let r = ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Поиск приложения")
                        .desired_width(w)
                        .margin(Margin::symmetric(18, 10)),
                );
                if r.changed() {
                    self.order_dirty = true;
                }
            });
            if chip(ui, "Все процессы", self.cfg.show_all, &p).clicked() {
                self.cfg.show_all = !self.cfg.show_all;
                self.mark_dirty();
            }
            if chip(ui, "По скорости", self.cfg.sort_by_speed, &p).clicked() {
                self.cfg.sort_by_speed = !self.cfg.sort_by_speed;
                self.mark_dirty();
            }
        });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        if self.order.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Нет приложений с сетевой активностью")
                        .size(16.0)
                        .color(p.on_variant),
                );
                ui.label(
                    RichText::new("Включите «Все процессы» или подождите, пока что-нибудь начнёт качать")
                        .small()
                        .color(p.on_variant),
                );
            });
            return;
        }
        let order = self.order.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for app in &order {
                    self.row(ui, app);
                }
                ui.add_space(8.0);
            });
    }

    fn row(&mut self, ui: &mut egui::Ui, app: &Arc<AppState>) {
        let p = self.pal;
        let key = app.name.to_lowercase();
        let speed = self.speed_of(app);
        self.rules.entry(key.clone()).or_insert_with(|| Rule::new(&app.name));

        let (limited, limit) = {
            let r = &self.rules[&key];
            (r.enabled && r.effective_bps() > 0, r.effective_bps())
        };
        let conns = app.conns.load(Ordering::Relaxed);

        let frame = Frame::new()
            .fill(if limited { p.container_high } else { p.container })
            .stroke(if limited { Stroke::new(1.0, p.primary_container) } else { Stroke::NONE })
            .corner_radius(16)
            .inner_margin(Margin::symmetric(16, 12));

        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // аватар
                let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                let (bg, fg) = p.avatar_colors(&key);
                ui.painter().circle_filled(rect.center(), 20.0, bg);
                let letter = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    letter,
                    FontId::proportional(18.0),
                    fg,
                );

                // название и скорость
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(egui::Label::new(RichText::new(&app.name).size(16.0)).truncate());
                    let mut sub = format!("↓ {}", fmt_speed(speed));
                    if conns > 0 {
                        sub.push_str(&format!(
                            "  ·  {} {}",
                            conns,
                            plural(conns, "соединение", "соединения", "соединений")
                        ));
                    }
                    ui.label(RichText::new(sub).small().color(p.on_variant));
                });

                // управление
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let rule = self.rules.get_mut(&key).unwrap();
                    let mut changed = false;

                    let mut on = rule.enabled;
                    if switch(ui, &mut on, &p).changed() {
                        rule.enabled = on;
                        changed = true;
                    }
                    ui.add_space(12.0);

                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let last = Unit::ALL.len() - 1;
                        for (i, unit) in Unit::ALL.iter().enumerate().rev() {
                            let corners = CornerRadius {
                                nw: if i == 0 { 16 } else { 0 },
                                sw: if i == 0 { 16 } else { 0 },
                                ne: if i == last { 16 } else { 0 },
                                se: if i == last { 16 } else { 0 },
                            };
                            if segment(ui, unit.label(), rule.unit == *unit, corners, &p).clicked()
                                && rule.unit != *unit
                            {
                                rule.unit = *unit;
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(12.0);

                    ui.scope(|ui| {
                        let invalid = !rule.text_valid();
                        if invalid {
                            let v = ui.visuals_mut();
                            v.widgets.inactive.bg_stroke = Stroke::new(1.5, p.error);
                            v.widgets.hovered.bg_stroke = Stroke::new(1.5, p.error);
                        }
                        let r = ui.add(
                            TextEdit::singleline(&mut rule.text)
                                .desired_width(64.0)
                                .align(Align2::CENTER_CENTER)
                                .margin(Margin::symmetric(8, 7)),
                        );
                        if r.changed() {
                            if let Some(v) = parse_value(&rule.text) {
                                rule.value = v;
                            }
                            changed = true;
                        }
                    });

                    if changed {
                        self.apply_rule(&key);
                        self.mark_dirty();
                    }
                });
            });

            if limited {
                ui.add_space(6.0);
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                let frac = (speed / limit as f64).clamp(0.0, 1.0) as f32;
                ui.painter().rect_filled(rect, CornerRadius::same(2), p.outline_variant);
                let mut fill = rect;
                fill.set_width(rect.width() * frac);
                ui.painter().rect_filled(fill, CornerRadius::same(2), p.primary);
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let total: f64 = self.stats.values().map(|s| s.speed).sum();
        let active = self
            .rules
            .values()
            .filter(|r| r.enabled && r.effective_bps() > 0)
            .count();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("↓ Всего {}", fmt_speed(total)))
                    .small()
                    .color(p.on_variant),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("Активных лимитов: {active}"))
                        .small()
                        .color(p.on_variant),
                );
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = self.pal;
        self.engine.tick();
        self.update_stats();
        if self.order_dirty || self.last_order.elapsed() > Duration::from_millis(1500) {
            self.rebuild_order();
        }
        if self.dirty_since.is_some_and(|t| t.elapsed() > Duration::from_millis(500)) {
            self.persist();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));

        egui::Panel::top("bar")
            .frame(Frame::new().fill(p.background).inner_margin(Margin::symmetric(24, 16)))
            .show_separator_line(false)
            .show(ui, |ui| self.top_bar(ui));

        egui::Panel::bottom("footer")
            .frame(Frame::new().fill(p.background).inner_margin(Margin::symmetric(24, 10)))
            .show_separator_line(false)
            .show(ui, |ui| self.footer(ui));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.background).inner_margin(Margin::symmetric(24, 0)))
            .show(ui, |ui| {
                self.error_banner(ui);
                self.toolbar(ui);
                ui.add_space(4.0);
                self.list(ui);
            });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.persist();
        self.shared.stop_refresher();
    }
}

// ----- виджеты в стиле Material 3 ---------------------------------------------------------------

/// Переключатель: дорожка 52×32 и «бегунок», который растёт при включении.
fn switch(ui: &mut egui::Ui, on: &mut bool, p: &Palette) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(52.0, 32.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let lerp = |a: Color32, b: Color32| a.lerp_to_gamma(b, t);

    let track = lerp(p.container_highest, p.primary);
    let border = lerp(p.outline, p.primary);
    ui.painter().rect(
        rect,
        CornerRadius::same(16),
        track,
        Stroke::new(2.0, border),
        StrokeKind::Inside,
    );
    let mut radius = 8.0 + 4.0 * t;
    if resp.is_pointer_button_down_on() {
        radius = 14.0;
    } else if resp.hovered() && t < 0.5 {
        radius = 10.0;
    }
    let x = egui::lerp((rect.left() + 16.0)..=(rect.right() - 16.0), t);
    ui.painter().circle_filled(
        egui::pos2(x, rect.center().y),
        radius,
        lerp(p.outline, p.on_primary),
    );
    resp
}

fn chip(ui: &mut egui::Ui, label: &str, selected: bool, p: &Palette) -> egui::Response {
    let (fill, stroke, text) = if selected {
        (p.secondary_container, Stroke::NONE, p.on_secondary_container)
    } else {
        (Color32::TRANSPARENT, Stroke::new(1.0, p.outline), p.on_variant)
    };
    ui.add(
        Button::new(RichText::new(label).color(text))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(8)
            .min_size(vec2(0.0, 32.0)),
    )
}

fn segment(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    corners: CornerRadius,
    p: &Palette,
) -> egui::Response {
    let (fill, text) = if selected {
        (p.secondary_container, p.on_secondary_container)
    } else {
        (Color32::TRANSPARENT, p.on_variant)
    };
    ui.add(
        Button::new(RichText::new(label).color(text).size(13.0))
            .fill(fill)
            .stroke(Stroke::new(1.0, p.outline))
            .corner_radius(corners)
            .min_size(vec2(0.0, 32.0)),
    )
}

// ----- форматирование -------------------------------------------------------------------------

fn fmt_number(v: f64) -> String {
    format!("{v}")
}

fn fmt_speed(bytes_per_sec: f64) -> String {
    let s = if bytes_per_sec >= 1024.0 * 1024.0 {
        format!("{:.1} МБ/с", bytes_per_sec / 1024.0 / 1024.0)
    } else if bytes_per_sec >= 1024.0 {
        format!("{:.0} КБ/с", bytes_per_sec / 1024.0)
    } else {
        format!("{:.0} Б/с", bytes_per_sec)
    };
    s.replace('.', ",")
}

fn plural(n: u32, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        one
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        few
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_speed(0.0), "0 Б/с");
        assert_eq!(fmt_speed(2048.0), "2 КБ/с");
        assert_eq!(fmt_speed(1.5 * 1024.0 * 1024.0), "1,5 МБ/с");
        assert_eq!(fmt_number(20.0), "20");
        assert_eq!(fmt_number(1.5), "1.5");
    }

    #[test]
    fn russian_plurals() {
        let f = |n| plural(n, "соединение", "соединения", "соединений");
        assert_eq!(f(1), "соединение");
        assert_eq!(f(3), "соединения");
        assert_eq!(f(5), "соединений");
        assert_eq!(f(11), "соединений");
        assert_eq!(f(21), "соединение");
        assert_eq!(f(12), "соединений");
    }

    #[test]
    fn rule_effective_limit() {
        let mut r = Rule::new("x.exe");
        assert_eq!(r.effective_bps(), 0); // выключено
        r.enabled = true;
        assert_eq!(r.effective_bps(), 1024 * 1024);
        r.text = "abc".into();
        assert_eq!(r.effective_bps(), 0); // некорректный ввод не даёт лимит
        r.text = "2,5".into();
        r.unit = Unit::KBps;
        assert_eq!(r.effective_bps(), 2560);
    }
}
