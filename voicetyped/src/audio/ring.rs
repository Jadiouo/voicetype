//! 環形緩衝區與 pre-roll (SDD §4.4)。
//!
//! 按下熱鍵到音訊串流實際開始有 50–100ms 落差, 第一個字容易被吃掉。
//! 解法是串流開啟後持續寫入環形緩衝區, `start` 事件到達時往回取一段
//! 音訊當作 pre-roll。
//!
//! 實作上用單調遞增的絕對樣本位置 (`written`) 當座標, 而不是讀寫指標:
//! 「往回 500ms」 直接就是 `written - preroll_samples`, 不必處理繞回的
//! 邊界情況。緩衝區存的是**原始取樣率**的 mono 音訊, 重採樣延到錄音
//! 結束後批次處理 (見 `super::resample`)。

/// 環形緩衝區。單一寫入者 (cpal 回呼), 單一讀取者 (session)。
pub struct RingBuffer {
    buf: Vec<f32>,
    /// 累計寫入的樣本總數。永遠遞增, 是所有位置計算的基準。
    written: u64,
}

impl RingBuffer {
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0, "ring buffer capacity must be non-zero");
        Self {
            buf: vec![0.0; capacity_samples],
            written: 0,
        }
    }

    /// 依取樣率與秒數建立。
    pub fn with_duration(sample_rate: u32, seconds: f32) -> Self {
        let n = (sample_rate as f32 * seconds).ceil() as usize;
        Self::new(n.max(1))
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// 目前累計寫入的樣本數。用來標記 session 的起點。
    pub fn written(&self) -> u64 {
        self.written
    }

    /// 尚未被覆蓋的最舊樣本位置。
    pub fn oldest_available(&self) -> u64 {
        self.written.saturating_sub(self.buf.len() as u64)
    }

    pub fn push(&mut self, samples: &[f32]) {
        let cap = self.buf.len();

        // 寫入量超過整個緩衝區時, 只有最後 cap 個樣本會留下。
        // 直接跳過前面的部分, 避免無謂的複製。
        let (samples, skipped) = if samples.len() > cap {
            let skip = samples.len() - cap;
            (&samples[skip..], skip)
        } else {
            (samples, 0)
        };
        self.written += skipped as u64;

        let start = (self.written % cap as u64) as usize;
        let first = (cap - start).min(samples.len());
        self.buf[start..start + first].copy_from_slice(&samples[..first]);
        if first < samples.len() {
            let rest = samples.len() - first;
            self.buf[..rest].copy_from_slice(&samples[first..]);
        }
        self.written += samples.len() as u64;
    }

    /// 取出從絕對位置 `from` 到現在的所有樣本。
    ///
    /// `from` 若早於仍留存的最舊樣本, 會被夾到 `oldest_available()` ——
    /// 錄音超過緩衝區長度時資料本來就丟了, 這裡回傳仍拿得到的部分,
    /// 由呼叫端 (60 秒上限檢查) 決定要不要當成錯誤。
    pub fn take_from(&self, from: u64) -> Vec<f32> {
        let cap = self.buf.len() as u64;
        let from = from.max(self.oldest_available()).min(self.written);
        let count = (self.written - from) as usize;
        if count == 0 {
            return Vec::new();
        }
        debug_assert!(count as u64 <= cap);

        let mut out = Vec::with_capacity(count);
        let start = (from % cap) as usize;
        let first = (self.buf.len() - start).min(count);
        out.extend_from_slice(&self.buf[start..start + first]);
        if first < count {
            out.extend_from_slice(&self.buf[..count - first]);
        }
        out
    }

    /// session 的起點: 現在的位置往回退 `preroll` 個樣本 (SDD §4.4)。
    pub fn mark_with_preroll(&self, preroll_samples: usize) -> u64 {
        self.written
            .saturating_sub(preroll_samples as u64)
            .max(self.oldest_available())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_take_within_capacity() {
        let mut r = RingBuffer::new(10);
        r.push(&[1.0, 2.0, 3.0]);
        assert_eq!(r.written(), 3);
        assert_eq!(r.take_from(0), vec![1.0, 2.0, 3.0]);
        assert_eq!(r.take_from(1), vec![2.0, 3.0]);
        assert_eq!(r.take_from(3), Vec::<f32>::new());
    }

    #[test]
    fn wraps_around() {
        let mut r = RingBuffer::new(4);
        r.push(&[1.0, 2.0, 3.0]);
        r.push(&[4.0, 5.0]); // 覆蓋掉 1.0
        assert_eq!(r.written(), 5);
        assert_eq!(r.oldest_available(), 1);
        assert_eq!(r.take_from(1), vec![2.0, 3.0, 4.0, 5.0]);
    }

    /// 起點早於仍留存的資料時要被夾住, 而不是回傳垃圾或 panic。
    #[test]
    fn clamps_stale_start_position() {
        let mut r = RingBuffer::new(4);
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(r.oldest_available(), 2);
        assert_eq!(r.take_from(0), vec![3.0, 4.0, 5.0, 6.0]);
    }

    /// 單次寫入超過整個緩衝區: 只保留最後 cap 個樣本, 且 written 仍要正確。
    #[test]
    fn push_larger_than_capacity() {
        let mut r = RingBuffer::new(3);
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r.written(), 5);
        assert_eq!(r.take_from(r.oldest_available()), vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn preroll_reaches_back_before_mark() {
        let mut r = RingBuffer::new(100);
        r.push(&[0.5; 50]); // 熱鍵按下前已經在錄
        let mark = r.mark_with_preroll(10);
        assert_eq!(mark, 40);
        r.push(&[1.0; 20]); // 使用者說話
        let captured = r.take_from(mark);
        assert_eq!(captured.len(), 30); // 10 pre-roll + 20 說話
        assert_eq!(captured[0], 0.5);
        assert_eq!(captured[29], 1.0);
    }

    /// 剛啟動、還沒累積到 pre-roll 長度時不能回傳未初始化的零。
    #[test]
    fn preroll_clamped_at_stream_start() {
        let mut r = RingBuffer::new(100);
        r.push(&[1.0; 5]);
        let mark = r.mark_with_preroll(10);
        assert_eq!(mark, 0);
        assert_eq!(r.take_from(mark).len(), 5);
    }

    /// pre-roll 不能回溯到已被覆蓋的區域。
    #[test]
    fn preroll_clamped_at_oldest_available() {
        let mut r = RingBuffer::new(10);
        r.push(&[1.0; 25]);
        let mark = r.mark_with_preroll(20);
        assert_eq!(mark, r.oldest_available());
        assert_eq!(r.take_from(mark).len(), 10);
    }

    #[test]
    fn with_duration_sizes_correctly() {
        let r = RingBuffer::with_duration(16_000, 30.0);
        assert_eq!(r.capacity(), 480_000);
    }

    /// 寫入量剛好等於容量的邊界。
    #[test]
    fn push_exactly_capacity() {
        let mut r = RingBuffer::new(4);
        r.push(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(r.written(), 4);
        assert_eq!(r.take_from(0), vec![1.0, 2.0, 3.0, 4.0]);
    }
}
