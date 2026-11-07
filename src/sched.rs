//! 按绝对拍数排的小日程。
//!
//! `BinaryHeap` 先弹出最早的那件。同一拍的顺序是固定的：
//! 定时器 0、1、2、3，然后行起点（垂直消隐、行匹配），然后水平消隐（含这时才开工的 DMA），最后声音采样。
//! 像素仍在经过的那一拍合成，不放到消隐里整行重画。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EventKind {
    Timer(u8),
    /// 一行的第一拍。第 160 行是垂直消隐。
    Line,
    /// 一行走到第 960 拍。
    HBlank,
    Sound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Wake {
    when: u64,
    rank: u8,
    seq: u64,
    generation: u32,
    kind: EventKind,
}

impl PartialOrd for Wake {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Wake {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .when
            .cmp(&self.when)
            .then(other.rank.cmp(&self.rank))
            .then(other.seq.cmp(&self.seq))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Scheduler {
    heap: BinaryHeap<Wake>,
    seq: u64,
    timer_gen: [u32; 4],
    line_gen: u32,
    hblank_gen: u32,
    sound_gen: u32,
    /// 下一次该醒的拍。热路径只比较这个数。排进更早的事、或弹出时才改。
    deadline: u64,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            seq: 0,
            timer_gen: [0; 4],
            line_gen: 0,
            hblank_gen: 0,
            sound_gen: 0,
            deadline: u64::MAX,
        }
    }

    pub(crate) fn deadline(&self) -> u64 {
        self.deadline
    }

    /// 取出 `when <= now` 的下一件有效事件。已经作废的世代直接丢掉。
    ///
    /// 弹出后，截止拍改成下一件还有效的时刻。排进更早的事时再提前。
    /// 关掉定时器不重算：如果它还占着堆顶，截止拍会偏早，下一次弹出把它丢掉。
    /// 偏早只会多醒一次，不会漏掉更晚的真事件。
    pub(crate) fn pop_due(&mut self, now: u64) -> Option<EventKind> {
        if self.deadline > now {
            return None;
        }
        self.drop_stale();
        let when = self.heap.peek().map(|wake| wake.when);
        let Some(when) = when else {
            self.deadline = u64::MAX;
            return None;
        };
        if when > now {
            self.deadline = when;
            return None;
        }
        let kind = self.heap.pop().unwrap().kind;
        self.drop_stale();
        self.deadline = self.heap.peek().map(|wake| wake.when).unwrap_or(u64::MAX);
        Some(kind)
    }

    pub(crate) fn schedule_timer(&mut self, index: u8, when: u64) {
        let generation = self.bump_timer(index);
        self.push(when, index, generation, EventKind::Timer(index));
    }

    pub(crate) fn silence_timer(&mut self, index: u8) {
        // 不在这里重算截止拍。作废的定时器如果还是堆顶，截止拍会偏早，
        // 下一次弹出把它丢掉。偏早只会多醒一次，不会跨过后面的真事件。
        self.bump_timer(index);
    }

    pub(crate) fn schedule_line(&mut self, when: u64) {
        self.line_gen = self.line_gen.wrapping_add(1);
        self.push(when, 4, self.line_gen, EventKind::Line);
    }

    pub(crate) fn schedule_hblank(&mut self, when: u64) {
        self.hblank_gen = self.hblank_gen.wrapping_add(1);
        self.push(when, 5, self.hblank_gen, EventKind::HBlank);
    }

    pub(crate) fn schedule_sound(&mut self, when: u64) {
        self.sound_gen = self.sound_gen.wrapping_add(1);
        self.push(when, 6, self.sound_gen, EventKind::Sound);
    }

    fn bump_timer(&mut self, index: u8) -> u32 {
        let slot = &mut self.timer_gen[index as usize];
        *slot = slot.wrapping_add(1);
        *slot
    }

    fn push(&mut self, when: u64, rank: u8, generation: u32, kind: EventKind) {
        self.seq = self.seq.wrapping_add(1);
        self.heap.push(Wake {
            when,
            rank,
            seq: self.seq,
            generation,
            kind,
        });
        if when < self.deadline {
            self.deadline = when;
        }
    }

    fn drop_stale(&mut self) {
        while let Some(wake) = self.heap.peek() {
            if self.live(wake) {
                break;
            }
            self.heap.pop();
        }
    }

    fn live(&self, wake: &Wake) -> bool {
        let generation = match wake.kind {
            EventKind::Timer(index) => self.timer_gen[index as usize],
            EventKind::Line => self.line_gen,
            EventKind::HBlank => self.hblank_gen,
            EventKind::Sound => self.sound_gen,
        };
        wake.generation == generation
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_cycle_runs_timer_before_sound() {
        let mut sched = Scheduler::new();
        sched.schedule_sound(100);
        sched.schedule_timer(1, 100);
        sched.schedule_timer(0, 100);
        sched.schedule_hblank(100);
        assert_eq!(sched.pop_due(100), Some(EventKind::Timer(0)));
        assert_eq!(sched.pop_due(100), Some(EventKind::Timer(1)));
        assert_eq!(sched.pop_due(100), Some(EventKind::HBlank));
        assert_eq!(sched.pop_due(100), Some(EventKind::Sound));
        assert_eq!(sched.pop_due(100), None);
    }

    #[test]
    fn silenced_timer_is_skipped() {
        let mut sched = Scheduler::new();
        sched.schedule_timer(0, 10);
        sched.silence_timer(0);
        sched.schedule_sound(10);
        assert_eq!(sched.pop_due(10), Some(EventKind::Sound));
    }

    #[test]
    fn deadline_is_the_earliest_live_event() {
        let mut sched = Scheduler::new();
        assert_eq!(sched.deadline(), u64::MAX);
        sched.schedule_sound(80);
        sched.schedule_hblank(40);
        assert_eq!(sched.deadline(), 40);
        sched.schedule_timer(0, 10);
        assert_eq!(sched.deadline(), 10);
        sched.silence_timer(0);
        assert_eq!(sched.pop_due(10), None);
        assert_eq!(sched.deadline(), 40);
        assert_eq!(sched.pop_due(40), Some(EventKind::HBlank));
        assert_eq!(sched.deadline(), 80);
    }

    #[test]
    fn a_later_reschedule_drops_the_old_deadline_on_pop() {
        let mut sched = Scheduler::new();
        sched.schedule_timer(0, 10);
        sched.schedule_timer(0, 40);
        assert_eq!(sched.pop_due(10), None);
        assert_eq!(sched.deadline(), 40);
        assert_eq!(sched.pop_due(40), Some(EventKind::Timer(0)));
    }
}
