//! 四路 16 位定时器。和画面共用累计周期，不读 `VCOUNT`。
//!
//! 分频是 1、64、256、1024，按累计周期的整段对齐。启动时不清掉这个相位。
//! 控制位第 2 位改成数前一路的溢出。0 号没有前一路，忽略这一位。
//! 计数绕过 `0xFFFF` 时回到重载值。第 6 位开着就把 `IF` 的第 3–6 位置上。

use serde::{Deserialize, Serialize};

const PRESCALE: [u64; 4] = [1, 64, 256, 1024];
const CONTROL_USED: u16 = 0x00C7;
const ENABLE: u16 = 1 << 7;
const IRQ: u16 = 1 << 6;
const CASCADE: u16 = 1 << 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Timer {
    counter: u16,
    reload: u16,
    control: u16,
    synced_at: u64,
}

impl Timer {
    fn new() -> Self {
        Self {
            counter: 0,
            reload: 0,
            control: 0,
            synced_at: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.control & ENABLE != 0
    }

    fn irq_enabled(&self) -> bool {
        self.control & IRQ != 0
    }

    fn cascade(&self) -> bool {
        self.control & CASCADE != 0
    }

    fn prescale_ticks(&self, now: u64) -> u64 {
        let step = PRESCALE[(self.control & 3) as usize];
        now / step - self.synced_at / step
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Timers {
    timer: [Timer; 4],
}

impl Timers {
    pub(crate) fn new() -> Self {
        Self {
            timer: [Timer::new(), Timer::new(), Timer::new(), Timer::new()],
        }
    }

    pub(crate) fn read_byte(&self, offset: usize, now: u64) -> u8 {
        let index = offset / 4;
        let counters = self.counters_at(now);
        match offset % 4 {
            0 => counters[index] as u8,
            1 => (counters[index] >> 8) as u8,
            2 => (self.timer[index].control & CONTROL_USED) as u8,
            _ => 0,
        }
    }

    pub(crate) fn write_byte(&mut self, offset: usize, value: u8, now: u64) {
        let index = offset / 4;
        match offset % 4 {
            0 => {
                let reload = &mut self.timer[index].reload;
                *reload = (*reload & 0xFF00) | u16::from(value);
            }
            1 => {
                let reload = &mut self.timer[index].reload;
                *reload = (*reload & 0x00FF) | (u16::from(value) << 8);
            }
            2 => {
                let was_running = self.timer[index].enabled();
                self.timer[index].control = u16::from(value) & CONTROL_USED;
                if !was_running && self.timer[index].enabled() {
                    self.timer[index].counter = self.timer[index].reload;
                    // 停着的时候拍数已经走掉了，启动从这一拍重新对齐。
                    self.timer[index].synced_at = now;
                }
            }
            _ => {}
        }
    }

    /// 下一次溢出的绝对拍。级联的那路没有自己的时刻，跟着前一路走。
    pub(crate) fn next_overflow(&self, index: usize, now: u64) -> Option<u64> {
        let timer = &self.timer[index];
        if !timer.enabled() || (index > 0 && timer.cascade()) {
            return None;
        }
        let step = PRESCALE[(timer.control & 3) as usize];
        let until = 0x10000u64 - u64::from(timer.counter);
        let already = timer.prescale_ticks(now);
        if already >= until {
            return Some(now);
        }
        let target_tick = now / step + (until - already);
        Some(target_tick * step)
    }

    /// 把四路计数追到 `now`。中断位或进 `IF`。前两项是定时器 0、1 的溢出次数，直接声按它取样本。
    pub(crate) fn materialize(&mut self, now: u64) -> (u16, [u64; 2]) {
        let mut mask = 0u16;
        let mut direct = [0u64; 2];
        let mut previous = 0u64;
        for (index, timer) in self.timer.iter_mut().enumerate() {
            let overflows = if timer.enabled() {
                let ticks = if index > 0 && timer.cascade() {
                    previous
                } else {
                    timer.prescale_ticks(now)
                };
                let (counter, count) = add_ticks(timer.counter, timer.reload, ticks);
                timer.counter = counter;
                if count > 0 && timer.irq_enabled() {
                    mask |= 1 << (3 + index);
                }
                count
            } else {
                0
            };
            timer.synced_at = now;
            if index < 2 {
                direct[index] = overflows;
            }
            previous = overflows;
        }
        (mask, direct)
    }

    fn counters_at(&self, now: u64) -> [u16; 4] {
        let mut counters = [0; 4];
        let mut previous = 0u64;
        for (index, timer) in self.timer.iter().enumerate() {
            if timer.enabled() {
                let ticks = if index > 0 && timer.cascade() {
                    previous
                } else {
                    timer.prescale_ticks(now)
                };
                let (counter, overflows) = add_ticks(timer.counter, timer.reload, ticks);
                counters[index] = counter;
                previous = overflows;
            } else {
                counters[index] = timer.counter;
                previous = 0;
            }
        }
        counters
    }
}

fn add_ticks(counter: u16, reload: u16, ticks: u64) -> (u16, u64) {
    if ticks == 0 {
        return (counter, 0);
    }
    let until = 0x10000u64 - u64::from(counter);
    if ticks < until {
        return ((u64::from(counter) + ticks) as u16, 0);
    }
    let left = ticks - until;
    let period = 0x10000u64 - u64::from(reload);
    let counter = u64::from(reload) + left % period;
    (counter as u16, 1 + left / period)
}
