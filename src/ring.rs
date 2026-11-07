// ring buffer

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct Ring<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
}

// 单生产者、单消费者环形缓冲
// 目前默认容量是 2 的幂，方便位运算
impl<T> Ring<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(1).next_power_of_two();
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(UnsafeCell::new(MaybeUninit::uninit()));
        }
        Self {
            slots: slots.into_boxed_slice(),
            mask: cap - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// 还没被取走的项数。
    pub(crate) fn len(&self) -> usize {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }
}

impl<T: Copy> Ring<T> {
    /// 放入一项。满了返回 `false`，原来的内容不动。
    pub(crate) fn push(&self, item: T) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) == self.capacity() {
            return false;
        }
        let index = write & self.mask;
        unsafe {
            (*self.slots[index].get()).write(item);
        }
        self.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// 取出最早的一项。空了就是 `None`。
    pub(crate) fn pop(&self) -> Option<T> {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let index = read & self.mask;
        let item = unsafe { (*self.slots[index].get()).assume_init_read() };
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(item)
    }

    /// 按写入顺序抄出还没取走的项，不移动读指针。
    ///
    /// 只能在没有消费者同时 `pop` 的时候用。播放线程在取的时候，不要再靠它看完整历史。
    pub(crate) fn pending(&self) -> Vec<T> {
        let read = self.read.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        let mut out = Vec::with_capacity(write.wrapping_sub(read));
        let mut at = read;
        while at != write {
            let index = at & self.mask;
            out.push(unsafe { (*self.slots[index].get()).assume_init_read() });
            at = at.wrapping_add(1);
        }
        out
    }
}

unsafe impl<T: Send> Send for Ring<T> {}
unsafe impl<T: Send> Sync for Ring<T> {}

impl<T> std::fmt::Debug for Ring<T> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("Ring")
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn a_full_ring_drops_the_new_item() {
        let ring = Ring::new(2);
        assert_eq!(ring.capacity(), 2);
        assert!(ring.push(1));
        assert!(ring.push(2));
        assert!(!ring.push(3));
        assert_eq!(ring.pending(), vec![1, 2]);
        assert_eq!(ring.pop(), Some(1));
        assert!(ring.push(3));
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn a_consumer_thread_reads_in_order() {
        let ring = Arc::new(Ring::new(64));
        let taken = Arc::clone(&ring);
        let consumer = thread::spawn(move || {
            let mut got = Vec::new();
            while got.len() < 100 {
                if let Some(item) = taken.pop() {
                    got.push(item);
                } else {
                    thread::yield_now();
                }
            }
            got
        });
        for item in 0..100 {
            while !ring.push(item) {
                thread::yield_now();
            }
        }
        assert_eq!(consumer.join().unwrap(), (0..100).collect::<Vec<_>>());
    }
}
