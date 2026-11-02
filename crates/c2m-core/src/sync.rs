//! Locks shared by the engine's threads.

use std::sync::{Mutex, MutexGuard};

/// 获取互斥锁；若锁已中毒（某线程在持锁期间 panic），直接取走数据继续用。
/// 语义：本进程自愿接受中毒锁——所有持锁临界区只做内存内状态更新，
/// 数据本身不会被 panic 路径写坏，传播 PoisonError 反而会让无关线程陪葬。
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
