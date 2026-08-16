//! 批注桥接：AnnotationStore（core 数据层）+ 当前文件批注列表 + 标记行集合。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use qview_core::annotation::{Annotation, AnnotationStore, MAX_SELECTED_SNAPSHOT};

pub struct Annotations {
    pub store: AnnotationStore,
    /// 当前打开文件的批注（按 start_byte 排序）
    pub list: Vec<Annotation>,
    /// 有批注的行号集合（渲染左侧琥珀条）
    pub marked: HashSet<u64>,
}

pub fn store_path() -> PathBuf {
    crate::settings::data_dir().join("annotations.json")
}

/// 本地时间 "YYYY-MM-DD HH:MM:SS.mmm"（批注创建时间）。
pub fn now() -> String {
    #[repr(C)]
    struct SystemTime {
        w_year: u16,
        w_month: u16,
        w_day_of_week: u16,
        w_day: u16,
        w_hour: u16,
        w_minute: u16,
        w_second: u16,
        w_milliseconds: u16,
    }
    extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    unsafe {
        let mut t = std::mem::zeroed::<SystemTime>();
        GetLocalTime(&mut t);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.w_year, t.w_month, t.w_day, t.w_hour, t.w_minute, t.w_second, t.w_milliseconds
        )
    }
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            store: AnnotationStore::load(&store_path()),
            list: Vec::new(),
            marked: HashSet::new(),
        }
    }
}

impl Annotations {
    pub fn load() -> Self {
        Self::default()
    }

    /// 换文件/重载后刷新当前文件批注与标记行。
    pub fn reload(&mut self, path: Option<&Path>) {
        self.list.clear();
        self.marked.clear();
        if let Some(p) = path {
            self.list = self.store.for_file(p).to_vec();
            for a in &self.list {
                for l in a.start_line..=a.end_line.min(a.start_line.saturating_add(100)) {
                    self.marked.insert(l);
                }
            }
        }
    }

    pub fn count(&self, path: &Path) -> usize {
        self.store.count(path)
    }

    /// 新增批注（截断 selected_text 快照到 4 KiB 且不截半字符）。
    pub fn add(&mut self, path: &Path, mut a: Annotation) -> u64 {
        if a.selected_text.len() > MAX_SELECTED_SNAPSHOT {
            let mut bytes = a.selected_text.as_bytes()[..MAX_SELECTED_SNAPSHOT].to_vec();
            while !std::str::from_utf8(&bytes).is_ok() {
                bytes.pop();
            }
            a.selected_text = String::from_utf8_lossy(&bytes).into_owned();
        }
        let id = self.store.add(path, a);
        let _ = self.store.save();
        id
    }

    pub fn remove(&mut self, path: &Path, id: u64) {
        if self.store.remove(path, id) {
            let _ = self.store.save();
        }
    }

    pub fn set_text(&mut self, path: &Path, id: u64, text: String) {
        if self.store.set_text(path, id, text) {
            let _ = self.store.save();
        }
    }
}
