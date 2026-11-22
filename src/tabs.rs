use crate::shell::resolve_shell;
use egui_term::{BackendSettings, PtyEvent, TerminalBackend};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};

pub struct Tab {
    pub backend: TerminalBackend,
    pub title: String,
}

pub struct Tabs {
    entries: BTreeMap<u64, Tab>,
    active: Option<u64>,
    next_id: u64,
    sender: Sender<(u64, PtyEvent)>,
    events: Receiver<(u64, PtyEvent)>,
}

impl Tabs {
    pub fn new() -> Self {
        let (sender, events) = mpsc::channel();
        Self {
            entries: BTreeMap::new(),
            active: None,
            next_id: 0,
            sender,
            events,
        }
    }

    pub fn add(
        &mut self,
        ctx: &egui::Context,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let id = self.next_id;
        let shell = resolve_shell(std::env::var("SHELL").ok().as_deref());
        let backend = TerminalBackend::new(
            id,
            ctx.clone(),
            self.sender.clone(),
            BackendSettings {
                shell,
                ..Default::default()
            },
        )
        .map_err(|err| {
            Box::new(std::io::Error::new(
                err.kind(),
                format!("failed to spawn terminal backend: {err}"),
            )) as Box<dyn std::error::Error + Send + Sync>
        })?;

        self.next_id += 1;
        self.entries.insert(
            id,
            Tab {
                backend,
                title: format!("shell {id}"),
            },
        );
        self.active = Some(id);
        Ok(id)
    }

    pub fn remove(&mut self, id: u64) {
        self.entries.remove(&id);
        self.active = select_active_after_removal(&self.ids(), id, self.active);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.active = None;
    }

    pub fn ids(&self) -> Vec<u64> {
        self.entries.keys().copied().collect()
    }

    pub fn active(&self) -> Option<u64> {
        self.active
    }

    pub fn set_active(&mut self, id: u64) {
        if self.entries.contains_key(&id) {
            self.active = Some(id);
        }
    }

    pub fn select_relative(&mut self, delta: isize) {
        self.active = select_relative(&self.ids(), self.active, delta);
    }

    pub fn title(&self, id: u64) -> Option<&str> {
        self.entries.get(&id).map(|tab| tab.title.as_str())
    }

    pub fn set_title(&mut self, id: u64, title: String) {
        if let Some(tab) = self.entries.get_mut(&id) {
            tab.title = title;
        }
    }

    pub fn tab_mut(&mut self, id: u64) -> Option<&mut Tab> {
        self.entries.get_mut(&id)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.active.and_then(|id| self.entries.get_mut(&id))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn try_iter(&self) -> impl Iterator<Item = (u64, PtyEvent)> + '_ {
        self.events.try_iter()
    }
}

pub fn select_active_after_removal(ids: &[u64], removed: u64, current: Option<u64>) -> Option<u64> {
    if ids.is_empty() {
        return None;
    }
    if current != Some(removed) {
        return current.filter(|id| ids.contains(id));
    }
    match ids.binary_search(&removed) {
        Ok(index) => ids
            .get(index + 1)
            .or_else(|| ids.get(index.wrapping_sub(1)))
            .copied(),
        Err(index) => ids
            .get(index)
            .or_else(|| ids.get(index.wrapping_sub(1)))
            .copied(),
    }
}

pub fn select_relative(ids: &[u64], current: Option<u64>, delta: isize) -> Option<u64> {
    if ids.is_empty() {
        return None;
    }
    let index = current
        .and_then(|id| ids.binary_search(&id).ok())
        .unwrap_or(0);
    let next = (index as isize + delta).rem_euclid(ids.len() as isize) as usize;
    ids.get(next).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tabs_left_means_no_active_tab() {
        assert_eq!(select_active_after_removal(&[], 0, Some(0)), None);
    }

    #[test]
    fn inactive_tab_keeps_current_selection() {
        assert_eq!(select_active_after_removal(&[0, 1], 0, Some(1)), Some(1));
    }

    #[test]
    fn removing_active_tab_selects_next() {
        assert_eq!(select_active_after_removal(&[1, 2], 0, Some(0)), Some(1));
    }

    #[test]
    fn removing_last_tab_selects_previous() {
        assert_eq!(select_active_after_removal(&[0, 1], 2, Some(2)), Some(1));
    }

    #[test]
    fn removing_middle_tab_selects_next() {
        assert_eq!(select_active_after_removal(&[0, 2], 1, Some(1)), Some(2));
    }

    #[test]
    fn relative_selection_moves_right() {
        assert_eq!(select_relative(&[0, 1, 2], Some(0), 1), Some(1));
    }

    #[test]
    fn relative_selection_moves_left() {
        assert_eq!(select_relative(&[0, 1, 2], Some(2), -1), Some(1));
    }

    #[test]
    fn relative_selection_wraps_right() {
        assert_eq!(select_relative(&[0, 1, 2], Some(2), 1), Some(0));
    }

    #[test]
    fn relative_selection_wraps_left() {
        assert_eq!(select_relative(&[0, 1, 2], Some(0), -1), Some(2));
    }

    #[test]
    fn relative_selection_without_tabs_returns_none() {
        assert_eq!(select_relative(&[], None, 1), None);
    }

    #[test]
    fn relative_selection_with_missing_current_starts_at_zero() {
        assert_eq!(select_relative(&[0, 1], Some(99), 1), Some(1));
    }
}
