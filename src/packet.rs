//! Минимальный разбор IP-заголовков: нам нужны только протокол и порт назначения
//! (для входящего пакета порт назначения — это локальный порт приложения).

pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub proto: u8,
    pub local_port: u16,
}

/// Возвращает протокол и порт назначения для входящего TCP/UDP-пакета (IPv4 или IPv6).
pub fn parse_inbound(buf: &[u8]) -> Option<Target> {
    let version = buf.first()? >> 4;
    let (proto, l4) = match version {
        4 => {
            let ihl = (buf[0] & 0x0f) as usize * 4;
            if ihl < 20 || buf.len() < ihl {
                return None;
            }
            // Не первый фрагмент: порта в нём нет.
            let frag_offset = u16::from_be_bytes([*buf.get(6)?, *buf.get(7)?]) & 0x1fff;
            if frag_offset != 0 {
                return None;
            }
            (*buf.get(9)?, ihl)
        }
        6 => {
            if buf.len() < 40 {
                return None;
            }
            // Пропускаем типичные расширения (hop-by-hop, routing, destination options).
            let mut next = buf[6];
            let mut off = 40usize;
            for _ in 0..4 {
                match next {
                    0 | 43 | 60 => {
                        let n = *buf.get(off)?;
                        let len = (*buf.get(off + 1)? as usize + 1) * 8;
                        next = n;
                        off += len;
                    }
                    _ => break,
                }
            }
            (next, off)
        }
        _ => return None,
    };
    if proto != PROTO_TCP && proto != PROTO_UDP {
        return None;
    }
    let p = buf.get(l4 + 2..l4 + 4)?;
    Some(Target {
        proto,
        local_port: u16::from_be_bytes([p[0], p[1]]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(proto: u8, dst_port: u16) -> Vec<u8> {
        let mut p = vec![0u8; 40];
        p[0] = 0x45;
        p[9] = proto;
        p[20 + 2..20 + 4].copy_from_slice(&dst_port.to_be_bytes());
        p
    }

    #[test]
    fn ipv4_tcp_and_udp() {
        assert_eq!(
            parse_inbound(&v4(6, 50123)),
            Some(Target { proto: 6, local_port: 50123 })
        );
        assert_eq!(
            parse_inbound(&v4(17, 443)),
            Some(Target { proto: 17, local_port: 443 })
        );
    }

    #[test]
    fn ipv4_with_options() {
        let mut p = vec![0u8; 60];
        p[0] = 0x46; // IHL = 6 -> 24 байта
        p[9] = 6;
        p[24 + 2..24 + 4].copy_from_slice(&1234u16.to_be_bytes());
        assert_eq!(parse_inbound(&p).unwrap().local_port, 1234);
    }

    #[test]
    fn ipv6_plain_and_with_extension() {
        let mut p = vec![0u8; 60];
        p[0] = 0x60;
        p[6] = 17;
        p[42..44].copy_from_slice(&5353u16.to_be_bytes());
        assert_eq!(parse_inbound(&p).unwrap().local_port, 5353);

        let mut q = vec![0u8; 80];
        q[0] = 0x60;
        q[6] = 0; // hop-by-hop
        q[40] = 6; // следующий заголовок — TCP
        q[41] = 0; // длина 8 байт
        q[48 + 2..48 + 4].copy_from_slice(&8080u16.to_be_bytes());
        assert_eq!(
            parse_inbound(&q),
            Some(Target { proto: 6, local_port: 8080 })
        );
    }

    #[test]
    fn rejects_garbage_and_icmp() {
        assert_eq!(parse_inbound(&[]), None);
        assert_eq!(parse_inbound(&[0x45, 0, 0]), None);
        assert_eq!(parse_inbound(&v4(1, 80)), None);
        let mut frag = v4(6, 80);
        frag[6] = 0x00;
        frag[7] = 0x10; // смещение фрагмента != 0
        assert_eq!(parse_inbound(&frag), None);
    }
}
