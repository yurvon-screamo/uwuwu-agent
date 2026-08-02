use std::path::Path;

use crate::chunk::TaskMeta;

const ISO_DATE_LEN: usize = 10;

#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub priority: Option<String>,
    pub deadline_from: Option<String>,
    pub deadline_to: Option<String>,
    pub designed: Option<bool>,
}

impl TaskQuery {
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.from.is_none()
            && self.to.is_none()
            && self.priority.is_none()
            && self.deadline_from.is_none()
            && self.deadline_to.is_none()
            && self.designed.is_none()
    }

    pub fn matches_default_exclude_closed(&self, path: &Path) -> bool {
        let meta = crate::chunk::read_task_meta(path);
        self.matches_meta(&meta, true)
    }

    pub fn matches_strict(&self, path: &Path) -> bool {
        let meta = crate::chunk::read_task_meta(path);
        self.matches_meta(&meta, false)
    }

    fn matches_meta(&self, meta: &TaskMeta, default_exclude_closed: bool) -> bool {
        match &self.status {
            Some(want_status) => match &meta.status {
                Some(s) if s.eq_ignore_ascii_case(want_status) => {}
                _ => return false,
            },
            None => {
                if default_exclude_closed
                    && meta
                        .status
                        .as_deref()
                        .map(|s| s.eq_ignore_ascii_case("closed"))
                        .unwrap_or(false)
                {
                    return false;
                }
            }
        }

        if let Some(want_priority) = &self.priority {
            match &meta.priority {
                Some(p) if p.eq_ignore_ascii_case(want_priority) => {}
                _ => return false,
            }
        }

        if let Some(want_designed) = &self.designed {
            match meta.designed {
                Some(d) if d == *want_designed => {}
                _ => return false,
            }
        }

        if let Some(d) = date_filter_passes(&meta.created, &self.from, &self.to) {
            if !d {
                return false;
            }
        }

        if let Some(d) = date_filter_passes(&meta.deadline, &self.deadline_from, &self.deadline_to)
        {
            if !d {
                return false;
            }
        }

        true
    }
}

fn date_filter_passes(
    value: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
) -> Option<bool> {
    if from.is_none() && to.is_none() {
        return None;
    }
    let v = match value.as_deref().map(|c| c.get(..ISO_DATE_LEN).unwrap_or(c)) {
        Some(v) => v,
        None => return Some(false),
    };
    if let Some(from_date) = from {
        if v < from_date.as_str() {
            return Some(false);
        }
    }
    if let Some(to_date) = to {
        if v > to_date.as_str() {
            return Some(false);
        }
    }
    Some(true)
}

pub fn priority_rank(p: Option<&str>) -> u8 {
    match p.map(|s| s.to_lowercase()).as_deref() {
        Some("high") => 0,
        Some("normal") => 1,
        Some("low") => 2,
        _ => 3,
    }
}

pub fn deadline_sort_key(deadline: &Option<String>) -> (u8, &str) {
    match deadline.as_deref() {
        Some(d) => (0, d.get(..ISO_DATE_LEN).unwrap_or(d)),
        None => (1, ""),
    }
}
