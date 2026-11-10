//! Общее состояние: приложения, соответствие «порт → приложение» и фоновое обновление.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// (протокол, локальный порт)
pub type PortKey = (u8, u16);

/// Приложение = все процессы с одним именем exe (у Chrome их десятки).
pub struct AppState {
    /// Имя как показываем пользователю (`chrome.exe`).
    pub name: String,
    /// Эффективный лимит в байтах/с, 0 — без ограничения. Пишет UI, читает движок.
    pub limit_bps: AtomicU64,
    /// Принято входящих байт с момента запуска (считает движок).
    pub bytes_in: AtomicU64,
    /// Отброшено шейпером байт.
    pub dropped: AtomicU64,
    /// Число установленных TCP-соединений.
    pub conns: AtomicU32,
}

impl AppState {
    fn new(name: String) -> Self {
        Self {
            name,
            limit_bps: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            conns: AtomicU32::new(0),
        }
    }
}

#[derive(Default)]
struct ProcTable {
    by_pid: HashMap<u32, Arc<AppState>>,
}

pub struct Shared {
    apps: RwLock<HashMap<String, Arc<AppState>>>,
    procs: RwLock<ProcTable>,
    sys: Mutex<System>,
    /// Заполняется событиями WinDivert (flow layer) — мгновенно.
    flow_ports: RwLock<HashMap<PortKey, Arc<AppState>>>,
    /// Заполняется периодическим обходом таблицы сокетов — подхватывает уже открытые соединения.
    sweep_ports: RwLock<HashMap<PortKey, Arc<AppState>>>,
    shutdown: AtomicBool,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            apps: RwLock::new(HashMap::new()),
            procs: RwLock::new(ProcTable::default()),
            sys: Mutex::new(System::new().expect("sysinfo не инициализировалась")),
            flow_ports: RwLock::new(HashMap::new()),
            sweep_ports: RwLock::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
        })
    }

    /// Находит приложение по имени или создаёт.
    pub fn app(&self, name: &str) -> Arc<AppState> {
        let key = name.to_lowercase();
        if let Some(a) = self.apps.read().unwrap().get(&key) {
            return a.clone();
        }
        let mut apps = self.apps.write().unwrap();
        apps.entry(key)
            .or_insert_with(|| Arc::new(AppState::new(name.to_string())))
            .clone()
    }

    pub fn apps_snapshot(&self) -> Vec<Arc<AppState>> {
        self.apps.read().unwrap().values().cloned().collect()
    }

    /// Приложение по PID. Если процесс появился между обходами — спрашиваем систему сразу.
    pub fn resolve_pid(&self, pid: u32) -> Option<Arc<AppState>> {
        if pid == 0 {
            return None;
        }
        if let Some(a) = self.procs.read().unwrap().by_pid.get(&pid) {
            return Some(a.clone());
        }
        let name = {
            let mut sys = self.sys.lock().unwrap();
            let p = sysinfo::Pid::from_u32(pid);
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[p]),
                false,
                ProcessRefreshKind::nothing(),
            );
            sys.process(p)?.name().to_string_lossy().into_owned()
        };
        let app = self.app(&name);
        self.procs.write().unwrap().by_pid.insert(pid, app.clone());
        Some(app)
    }

    pub fn lookup_port(&self, proto: u8, port: u16) -> Option<Arc<AppState>> {
        let key = (proto, port);
        if let Some(a) = self.flow_ports.read().unwrap().get(&key) {
            return Some(a.clone());
        }
        self.sweep_ports.read().unwrap().get(&key).cloned()
    }

    pub fn flow_open(&self, proto: u8, port: u16, app: Arc<AppState>) {
        self.flow_ports.write().unwrap().insert((proto, port), app);
    }

    pub fn flow_close(&self, proto: u8, port: u16) {
        self.flow_ports.write().unwrap().remove(&(proto, port));
    }

    /// Очищает события потоков (при остановке движка).
    pub fn clear_flows(&self) {
        self.flow_ports.write().unwrap().clear();
    }

    /// Один проход: список процессов + таблица сокетов.
    pub fn refresh(&self) {
        // 1. процессы
        let by_pid: HashMap<u32, String> = {
            let mut sys = self.sys.lock().unwrap();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            sys.processes()
                .iter()
                .filter(|(pid, _)| pid.as_u32() != 0)
                .map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().into_owned()))
                .collect()
        };
        let mut table = ProcTable::default();
        for (pid, name) in &by_pid {
            table.by_pid.insert(*pid, self.app(name));
        }

        // 2. сокеты
        let mut ports: HashMap<PortKey, Arc<AppState>> = HashMap::new();
        let mut conns: HashMap<usize, u32> = HashMap::new();
        let mut seen: HashSet<usize> = HashSet::new();
        let sockets = netstat2::get_sockets_info(
            AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
            ProtocolFlags::TCP | ProtocolFlags::UDP,
        )
        .unwrap_or_default();
        for s in sockets {
            let Some(app) = s
                .associated_pids
                .iter()
                .find_map(|pid| table.by_pid.get(pid))
                .cloned()
            else {
                continue;
            };
            let id = Arc::as_ptr(&app) as usize;
            seen.insert(id);
            match &s.protocol_socket_info {
                ProtocolSocketInfo::Tcp(t) => {
                    if t.state == TcpState::Established {
                        *conns.entry(id).or_default() += 1;
                    }
                    ports.insert((crate::packet::PROTO_TCP, t.local_port), app);
                }
                ProtocolSocketInfo::Udp(u) => {
                    ports.insert((crate::packet::PROTO_UDP, u.local_port), app);
                }
            }
        }
        for app in self.apps_snapshot() {
            let id = Arc::as_ptr(&app) as usize;
            app.conns
                .store(conns.get(&id).copied().unwrap_or(0), Ordering::Relaxed);
        }

        *self.procs.write().unwrap() = table;
        *self.sweep_ports.write().unwrap() = ports;
    }

    pub fn stop_refresher(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Фоновый поток, раз в ~1.5 с обновляющий процессы и сокеты.
pub fn spawn_refresher(shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("refresher".into())
        .spawn(move || {
            while !shared.shutdown.load(Ordering::Relaxed) {
                shared.refresh();
                std::thread::sleep(Duration::from_millis(1500));
            }
        })
        .expect("не удалось запустить поток обновления");
}
