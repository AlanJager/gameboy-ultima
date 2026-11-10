//! 已经解开的 e-Reader 卡。不扫描点码，也不带 e-Reader 启动卡带。
//!
//! 超级马力欧 Advance 4 做主机，从多人联机口要卡。这边占 2 号位。
//! 字是上一拍放进发送寄存器的：这一拍先送出它，再按主机刚送来的字决定下一拍。

const HEADER: usize = 0x72;
const CARD_LEN: usize = 2112;
const CONTENTS: usize = CARD_LEN - HEADER;

const HANDSHAKE_1: u16 = 0xFBFB;
const HANDSHAKE_2: u16 = 0x5841;
const HANDSHAKE_3: u16 = 0x4534;
const GAME_ANIMATING: u16 = 0xF3F3;
const EREADER_ANIMATING: u16 = 0xF2F2;
const EREADER_READY: u16 = 0xF1F1;
const GAME_READY_DEMO: u16 = 0xEFEF;
const GAME_READY_POWERUP: u16 = 0xF0F0;
const GAME_READY_LEVEL: u16 = 0xFAFA;
const EREADER_SEND_READY: u16 = 0xF9F9;
const GAME_RECEIVE_READY: u16 = 0xFEFE;
const EREADER_SEND_START: u16 = 0xFDFD;
const EREADER_SEND_END: u16 = 0xFCFC;
const GAME_RECEIVE_ERROR: u16 = 0xF4F4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Handshake1,
    Handshake2,
    Handshake3,
    WaitRequest,
    Animating,
    Pulse,
    Ready,
    SendReady,
    Mark,
    Payload,
    SumLow,
    SumHigh,
    End,
    Idle,
}

#[derive(Clone, Debug)]
pub(crate) struct EReader {
    card: Vec<u8>,
    armed: u16,
    phase: Phase,
    offset: usize,
    checksum: u32,
}

impl Default for EReader {
    fn default() -> Self {
        Self {
            card: Vec::new(),
            armed: 0,
            phase: Phase::Handshake1,
            offset: HEADER,
            checksum: 0,
        }
    }
}

impl EReader {
    pub(crate) fn loaded(&self) -> bool {
        self.card.len() == CARD_LEN
    }

    pub(crate) fn load(&mut self, bytes: &[u8]) -> Result<String, String> {
        let card = normalize(bytes)?;
        let line = label(&card);
        self.card = card;
        self.restart();
        Ok(line)
    }

    pub(crate) fn clear(&mut self) {
        self.card.clear();
        self.restart();
    }

    /// 这一拍 2 号位送出的字。没载卡时不占这个位子。
    pub(crate) fn answer(&mut self, parent: u16) -> Option<u16> {
        if !self.loaded() {
            return None;
        }
        let out = self.armed;
        self.react(parent);
        Some(out)
    }

    fn restart(&mut self) {
        self.armed = 0;
        self.phase = Phase::Handshake1;
        self.offset = HEADER;
        self.checksum = 0;
    }

    fn react(&mut self, parent: u16) {
        match self.phase {
            Phase::Handshake1 => {
                self.armed = HANDSHAKE_1;
                if parent == HANDSHAKE_1 {
                    self.phase = Phase::Handshake2;
                }
            }
            Phase::Handshake2 => {
                self.armed = HANDSHAKE_2;
                if parent == HANDSHAKE_2 {
                    self.phase = Phase::Handshake3;
                }
            }
            Phase::Handshake3 => {
                self.armed = HANDSHAKE_3;
                if parent == HANDSHAKE_3 {
                    self.phase = Phase::WaitRequest;
                }
            }
            Phase::WaitRequest => {
                self.armed = HANDSHAKE_3;
                if parent != HANDSHAKE_3 {
                    self.phase = Phase::Animating;
                }
            }
            Phase::Animating => {
                self.armed = GAME_ANIMATING;
                if parent == EREADER_ANIMATING {
                    self.phase = Phase::Pulse;
                }
            }
            Phase::Pulse => {
                self.armed = EREADER_ANIMATING;
                self.phase = Phase::Ready;
            }
            Phase::Ready => {
                self.armed = EREADER_READY;
                if matches!(parent, GAME_READY_DEMO | GAME_READY_POWERUP | GAME_READY_LEVEL) {
                    self.phase = Phase::SendReady;
                }
            }
            Phase::SendReady => {
                self.armed = EREADER_SEND_READY;
                if parent == GAME_RECEIVE_READY {
                    self.phase = Phase::Mark;
                    self.offset = HEADER;
                    self.checksum = 0;
                }
            }
            Phase::Mark => {
                self.armed = EREADER_SEND_START;
                self.phase = Phase::Payload;
            }
            Phase::Payload => {
                let word = self.half();
                self.armed = word;
                self.checksum = self.checksum.wrapping_add(u32::from(word));
                self.offset += 2;
                if self.offset >= CARD_LEN {
                    self.phase = Phase::SumLow;
                }
            }
            Phase::SumLow => {
                self.armed = self.checksum as u16;
                self.phase = Phase::SumHigh;
            }
            Phase::SumHigh => {
                self.armed = (self.checksum >> 16) as u16;
                self.phase = Phase::End;
            }
            Phase::End => {
                self.armed = EREADER_SEND_END;
                self.phase = Phase::Idle;
            }
            Phase::Idle => {
                self.armed = 0;
                if parent == HANDSHAKE_1 || parent == GAME_RECEIVE_ERROR {
                    self.restart();
                }
            }
        }
    }

    fn half(&self) -> u16 {
        let lo = self.card.get(self.offset).copied().unwrap_or(0);
        let hi = self.card.get(self.offset + 1).copied().unwrap_or(0);
        u16::from(lo) | (u16::from(hi) << 8)
    }
}

pub fn ereader_label(bytes: &[u8]) -> Result<String, String> {
    Ok(label(&normalize(bytes)?))
}

fn normalize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut card = vec![0u8; CARD_LEN];
    match bytes.len() {
        CARD_LEN => card.copy_from_slice(bytes),
        CONTENTS => card[HEADER..].copy_from_slice(bytes),
        _ => {
            return Err("要已经解开的 .bin，2112 字节，或者只要 1998 字节内容".into());
        }
    }
    Ok(card)
}

fn label(card: &[u8]) -> String {
    let number = card[HEADER];
    match card[HEADER + 1] {
        0x00 => format!("e-Reader · 关卡 A-{number:03}"),
        0x04 => format!("e-Reader · 道具 C-{number:03}"),
        0x06 => format!("e-Reader · 道具 D-{number:03}"),
        0x08 => format!("e-Reader · 演示 E-{number:03}"),
        _ => "e-Reader 卡已载入".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::ereader_label;
    use crate::bus::Bus;

    const SIOCNT: u32 = 0x0400_0128;
    const SEND: u32 = 0x0400_012A;
    const MULTI: u32 = 0x0400_0120;

    fn start_multi(bus: &mut Bus) {
        bus.write_u16(SIOCNT, (1 << 13) | 3 | (1 << 7));
    }

    fn exchange(bus: &mut Bus, parent: u16) -> u16 {
        bus.write_u16(SEND, parent);
        start_multi(bus);
        bus.advance(20_000);
        assert_eq!(bus.read_u16(SIOCNT) & 0x80, 0, "transfer did not finish");
        assert_eq!(bus.read_u16(MULTI + 4), 0xFFFF);
        assert_eq!(bus.read_u16(MULTI + 6), 0xFFFF);
        bus.read_u16(MULTI + 2)
    }

    fn until(bus: &mut Bus, parent: u16, want: u16) {
        let mut seen = 0u16;
        for _ in 0..16 {
            seen = exchange(bus, parent);
            if seen == want {
                return;
            }
        }
        panic!("wanted {want:04X}, last child word was {seen:04X}");
    }

    fn sample_card() -> Vec<u8> {
        let mut raw = vec![0u8; 2112];
        raw[0x72] = 0x07;
        raw[0x73] = 0x00;
        raw[0x74] = 0xCE;
        raw[0x75] = 0xCF;
        for (index, byte) in raw.iter_mut().enumerate().skip(0x76) {
            *byte = (index as u8).wrapping_mul(3);
        }
        raw
    }

    #[test]
    fn labels_follow_the_set_type_byte() {
        fn card(kind: u8) -> Vec<u8> {
            let mut raw = vec![0u8; 2112];
            raw[0x72] = 12;
            raw[0x73] = kind;
            raw
        }
        assert_eq!(ereader_label(&card(0x00)).unwrap(), "e-Reader · 关卡 A-012");
        assert_eq!(ereader_label(&card(0x04)).unwrap(), "e-Reader · 道具 C-012");
        assert_eq!(ereader_label(&card(0x06)).unwrap(), "e-Reader · 道具 D-012");
        assert_eq!(ereader_label(&card(0x08)).unwrap(), "e-Reader · 演示 E-012");
        assert_eq!(ereader_label(&card(0x03)).unwrap(), "e-Reader 卡已载入");
        assert!(ereader_label(&vec![0; 10]).unwrap_err().contains("2112"));
        let mut contents = vec![0u8; 1998];
        contents[0] = 4;
        contents[1] = 0x08;
        assert_eq!(ereader_label(&contents).unwrap(), "e-Reader · 演示 E-004");
    }

    #[test]
    fn a_loaded_card_takes_the_two_player_shift() {
        let mut bare = Bus::new();
        bare.write_u16(SEND, 1);
        start_multi(&mut bare);
        bare.advance(6_000);
        assert_eq!(bare.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(bare.read_u16(MULTI + 2), 0xFFFF);

        let mut bus = Bus::new();
        let line = bus.load_ereader_card(&vec![0; 2112]).unwrap();
        assert_eq!(line, "e-Reader · 关卡 A-000");
        bus.write_u16(SIOCNT, (1 << 13) | 3);
        assert_ne!(bus.read_u16(SIOCNT) & (1 << 3), 0);
        assert_eq!(bus.read_u16(SIOCNT) & 0x30, 0);
        bus.write_u16(SEND, 1);
        start_multi(&mut bus);
        bus.advance(6_000);
        assert_ne!(bus.read_u16(SIOCNT) & 0x80, 0, "two players should still be shifting");
        bus.advance(4_000);
        assert_eq!(bus.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(bus.read_u16(MULTI), 1);
        assert_eq!(bus.read_u16(MULTI + 2), 0);
        assert_eq!(bus.read_u16(MULTI + 4), 0xFFFF);
        assert_eq!(bus.read_u16(MULTI + 6), 0xFFFF);
    }

    #[test]
    fn a_card_follows_the_advance_4_handshake() {
        let raw = sample_card();
        let mut bus = Bus::new();
        assert_eq!(
            bus.load_ereader_card(&raw).unwrap(),
            "e-Reader · 关卡 A-007"
        );

        until(&mut bus, 0xFBFB, 0xFBFB);
        until(&mut bus, 0x5841, 0x5841);
        until(&mut bus, 0x4534, 0x4534);
        until(&mut bus, 0xEEEE, 0xF3F3);
        until(&mut bus, 0xF2F2, 0xF2F2);
        until(&mut bus, 0xFAFA, 0xF1F1);
        until(&mut bus, 0xFEFE, 0xF9F9);
        until(&mut bus, 0xFEFE, 0xFDFD);

        let mut sum = 0u32;
        for offset in (0x72..2112).step_by(2) {
            let word = u16::from(raw[offset]) | (u16::from(raw[offset + 1]) << 8);
            assert_eq!(exchange(&mut bus, 0), word, "payload at {offset:#X}");
            sum += u32::from(word);
        }
        assert_eq!(exchange(&mut bus, 0), sum as u16);
        assert_eq!(exchange(&mut bus, 0), (sum >> 16) as u16);
        assert_eq!(exchange(&mut bus, 0), 0xFCFC);

        until(&mut bus, 0xFBFB, 0xFBFB);
    }

    #[test]
    fn contents_without_a_header_are_sent_as_is() {
        let mut contents = vec![0u8; 1998];
        contents[0] = 0x11;
        contents[1] = 0x08;
        contents[2] = 0xCE;
        contents[3] = 0xCF;
        let mut bus = Bus::new();
        assert_eq!(
            bus.load_ereader_card(&contents).unwrap(),
            "e-Reader · 演示 E-017"
        );
        until(&mut bus, 0xFBFB, 0xFBFB);
        until(&mut bus, 0x5841, 0x5841);
        until(&mut bus, 0x4534, 0x4534);
        until(&mut bus, 0xEDED, 0xF3F3);
        until(&mut bus, 0xF2F2, 0xF2F2);
        until(&mut bus, 0xF0F0, 0xF1F1);
        until(&mut bus, 0xFEFE, 0xF9F9);
        until(&mut bus, 0xFEFE, 0xFDFD);
        assert_eq!(exchange(&mut bus, 0), 0x0811);
        assert_eq!(exchange(&mut bus, 0), 0xCFCE);
    }

    #[test]
    fn a_real_link_keeps_the_card_quiet() {
        let mut bus = Bus::new();
        bus.load_ereader_card(&sample_card()).unwrap();
        bus.set_link_player(Some(0));
        bus.write_u16(SEND, 0xFBFB);
        start_multi(&mut bus);
        bus.advance(20_000);
        assert_ne!(bus.read_u16(SIOCNT) & 0x80, 0);
        assert_eq!(bus.poll_link_need(), Some(0xFBFB));
        bus.set_link_player(None);
        assert_eq!(exchange(&mut bus, 0xFBFB), 0);
    }

    #[test]
    fn clearing_the_card_leaves_the_seat_empty() {
        let mut bus = Bus::new();
        bus.load_ereader_card(&vec![0; 1998]).unwrap();
        bus.clear_ereader_card();
        bus.write_u16(SEND, 0x1234);
        start_multi(&mut bus);
        bus.advance(8_000);
        assert_eq!(bus.read_u16(MULTI), 0x1234);
        assert_eq!(bus.read_u16(MULTI + 2), 0xFFFF);
        assert_eq!(bus.read_u16(SIOCNT) & (1 << 3), 0);
    }
}
