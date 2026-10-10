//! The window. One tab per open project, in the manner of a browser, and the state
//! every tab shares: the settings, nerd mode, the last copy, and the quit.
//!
//! Each tab is an `App`, which owns one project's sessions and both of its panes.
//! The browser routes a key, a click, or a paste to the focused tab, and handles the
//! few keys that concern tabs itself.

use std::sync::mpsc::Receiver;

use anyhow::{Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::app::{App, Deps};
use crate::config::Config;
use crate::hooks::HookEvent;
use crate::keys::{HostAction, host_action};
use crate::project::{Project, ProjectEntry};
use crate::stats::MemoryProbe;
use crate::ui;

/// The most tabs one window holds. `Alt+1` to `Alt+8` address them.
pub const MAX_TABS: usize = crate::workspace::MAX_PROJECTS;

/// One open project, and the channel its hooks arrive on.
#[derive(Debug)]
pub struct Tab {
    app: App,
    hooks: Option<Receiver<HookEvent>>,
}

impl Tab {
    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }
}

/// The workspace a window was opened from, and the projects it held at the time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOrigin {
    pub name: String,
    pub projects: Vec<String>,
}

/// The list of registered projects that `Alt+Shift+T` offers.
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub filter: String,
    /// The highlighted row, counted among the matches of `filter`.
    pub cursor: usize,
    /// Why the last pick did not open. Cleared by the next key.
    pub notice: Option<String>,
    candidates: Vec<ProjectEntry>,
}

impl Picker {
    /// The projects whose name holds the filter, ignoring case, in registry order.
    #[must_use]
    pub fn matches(&self) -> Vec<&ProjectEntry> {
        let needle = self.filter.to_lowercase();
        self.candidates
            .iter()
            .filter(|p| needle.is_empty() || p.slug.to_lowercase().contains(&needle))
            .collect()
    }
}

/// One answer the save prompt offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    /// Write the open projects into the workspace the window came from.
    Update,
    SaveAsNew,
    DontSave,
}

/// The question a quit asks when the open projects are worth keeping as a workspace.
#[derive(Debug, Clone)]
pub struct SavePrompt {
    pub choices: Vec<SaveChoice>,
    pub cursor: usize,
    /// The name being typed, once "save as new" was picked.
    pub name: Option<String>,
    /// Why the last save was refused.
    pub error: Option<String>,
}

/// A form that belongs to the window rather than to one tab.
#[derive(Debug, Clone)]
pub enum BrowserModal {
    Picker(Picker),
    SavePrompt(SavePrompt),
}

/// Where the tabs sit, refreshed on every draw, and the focused tab's own hit box.
#[derive(Debug, Clone, Default)]
pub struct BrowserHit {
    pub tabs: Vec<(Rect, usize)>,
    pub app: ui::HitBox,
}

/// True when a quit must ask whether to keep the open projects as a workspace.
///
/// A window opened from a workspace asks only when the set of projects changed. A
/// new order of the same projects is not a change. Any other window asks when it
/// holds more than one project.
#[must_use]
pub fn needs_save_prompt(origin: Option<&WorkspaceOrigin>, open: &[String]) -> bool {
    match origin {
        None => open.len() > 1,
        Some(origin) => {
            let mut was = origin.projects.clone();
            let mut now = open.to_vec();
            was.sort();
            was.dedup();
            now.sort();
            now.dedup();
            was != now
        }
    }
}

#[derive(Debug)]
pub struct Browser {
    tabs: Vec<Tab>,
    focused: usize,
    /// Tabs closed with `Alt+Shift+W` whose children are still leaving.
    closing_tabs: Vec<App>,
    origin: Option<WorkspaceOrigin>,
    modal: Option<BrowserModal>,
    /// True once a quit went past the save prompt and every tab was asked to close.
    quitting: bool,
    quit: bool,
    /// The whole terminal, kept so a tab opened later is sized before its first draw.
    area: Rect,
    config: Config,
    nerd_mode: bool,
    keyboard_enhanced: bool,
    hooks_installed: bool,
    clipboard: String,
}

impl Browser {
    /// Opens one tab per project, in order, and focuses the first.
    ///
    /// A project that cannot open, because another culm holds it, is skipped and
    /// reported on the first tab's bar. When no project opens, nothing is shown and
    /// the reasons come back as the error.
    pub fn open(
        projects: Vec<Project>,
        origin: Option<WorkspaceOrigin>,
        area: Rect,
        config: Config,
        deps: &Deps<'_>,
    ) -> Result<Self> {
        let mut browser = Self {
            tabs: Vec::new(),
            focused: 0,
            closing_tabs: Vec::new(),
            origin,
            modal: None,
            quitting: false,
            quit: false,
            area,
            config,
            nerd_mode: false,
            keyboard_enhanced: false,
            hooks_installed: true,
            clipboard: String::new(),
        };
        let mut refused = Vec::new();
        for project in projects.into_iter().take(MAX_TABS) {
            if let Err(e) = browser.add_tab(project, deps) {
                refused.push(e.to_string());
            }
        }
        if browser.tabs.is_empty() {
            bail!("{}", refused.join(". "));
        }
        if let Some(tab) = browser.tabs.first_mut()
            && !refused.is_empty()
        {
            tab.app.notify(refused.join(". "));
        }
        Ok(browser)
    }

    /// Opens a project as the last tab. Fails, and opens nothing, when another culm
    /// holds the project.
    fn add_tab(&mut self, project: Project, deps: &Deps<'_>) -> Result<()> {
        let hooks = deps.hooks.listen(&project.slug)?;
        let (rows, cols) = self.panel_size_for(project.sidebar_width());
        let mut app = App::open(project, deps, rows, cols);
        app.set_config(self.config.clone());
        app.set_nerd_mode(self.nerd_mode);
        app.set_keyboard_enhanced(self.keyboard_enhanced);
        app.set_hooks_installed(self.hooks_installed);
        app.set_clipboard(self.clipboard.clone());
        self.tabs.push(Tab {
            app,
            hooks: Some(hooks),
        });
        Ok(())
    }

    /// The panel of a tab whose sidebar is `sidebar_width` wide, in rows and columns.
    fn panel_size_for(&self, sidebar_width: u16) -> (u16, u16) {
        let (_, body) = ui::split_tab_bar(self.area);
        ui::panel_size(ui::layout(body, sidebar_width).panel)
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    #[must_use]
    pub fn focused(&self) -> usize {
        self.focused
    }

    /// The project on screen.
    #[must_use]
    pub fn app(&self) -> &App {
        // `open` refuses to build a window with no tab, and a close never takes the
        // last one, so the index always holds a tab.
        &self.tabs[self.focused].app
    }

    #[must_use]
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.tabs[self.focused].app
    }

    /// The slugs of the open projects, in tab order.
    #[must_use]
    pub fn open_projects(&self) -> Vec<String> {
        self.tabs
            .iter()
            .map(|t| t.app.project().slug.clone())
            .collect()
    }

    #[must_use]
    pub fn origin(&self) -> Option<&WorkspaceOrigin> {
        self.origin.as_ref()
    }

    #[must_use]
    pub fn modal(&self) -> Option<&BrowserModal> {
        self.modal.as_ref()
    }

    /// True while every tab is closing. The screen shows the spinner.
    #[must_use]
    pub fn quitting(&self) -> bool {
        self.quitting
    }

    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// How many closed tabs are still waiting for their children.
    #[must_use]
    pub fn closing_tabs(&self) -> usize {
        self.closing_tabs.len()
    }

    pub fn set_keyboard_enhanced(&mut self, enhanced: bool) {
        self.keyboard_enhanced = enhanced;
        for tab in &mut self.tabs {
            tab.app.set_keyboard_enhanced(enhanced);
        }
    }

    pub fn set_hooks_installed(&mut self, installed: bool) {
        self.hooks_installed = installed;
        for tab in &mut self.tabs {
            tab.app.set_hooks_installed(installed);
        }
    }

    /// The event loop measures the frame rate. The focused tab renders it.
    pub fn set_fps(&mut self, fps: f64) {
        self.app_mut().set_fps(fps);
    }

    /// Focuses a tab. An index past the last tab leaves the focus alone.
    pub fn focus_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.focused = index;
        }
    }

    pub fn on_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        if self.quitting {
            // Only a second quit means anything now: it stops the wait.
            if host_action(key) == Some(HostAction::Quit) {
                self.force_quit();
            }
            return Ok(());
        }
        match self.modal {
            Some(BrowserModal::Picker(_)) => return self.picker_key(key, deps),
            Some(BrowserModal::SavePrompt(_)) => return self.save_key(key, deps),
            None => {}
        }
        match host_action(key) {
            Some(HostAction::Quit) => self.request_quit(deps),
            Some(HostAction::FocusTab(index)) => self.focus_tab(index),
            Some(HostAction::NewTab) => self.open_picker(deps),
            Some(HostAction::CloseTab) => self.close_tab(deps),
            _ => {
                self.app_mut().on_key(key, deps)?;
                self.share_from_focused();
            }
        }
        Ok(())
    }

    /// Carries a change one tab made to what every tab shares into the others.
    fn share_from_focused(&mut self) {
        let config = self.app().config().clone();
        if config != self.config {
            self.config = config;
            for tab in &mut self.tabs {
                tab.app.set_config(self.config.clone());
            }
        }
        let nerd = self.app().nerd_mode();
        if nerd != self.nerd_mode {
            self.nerd_mode = nerd;
            for tab in &mut self.tabs {
                tab.app.set_nerd_mode(nerd);
            }
        }
    }

    pub fn on_paste(&mut self, text: &str) -> Result<()> {
        match self.modal.as_mut() {
            Some(BrowserModal::Picker(picker)) => {
                picker.filter.push_str(text);
                picker.cursor = 0;
                Ok(())
            }
            Some(BrowserModal::SavePrompt(prompt)) => {
                if let Some(name) = prompt.name.as_mut() {
                    name.push_str(text);
                }
                Ok(())
            }
            None if self.quitting => Ok(()),
            None => self.app_mut().on_paste(text),
        }
    }

    /// A press on a tab focuses it. Everything else reaches the focused tab, a
    /// release over the tab bar included, so a drag never stays half done.
    pub fn on_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        hit: &BrowserHit,
        deps: &Deps<'_>,
    ) {
        if self.quitting || self.modal.is_some() {
            return;
        }
        let at = Position { x: column, y: row };
        if let MouseEventKind::Down(MouseButton::Left) = kind
            && let Some(&(_, index)) = hit.tabs.iter().find(|(rect, _)| rect.contains(at))
        {
            self.focus_tab(index);
            return;
        }
        self.app_mut()
            .on_mouse_with(kind, column, row, modifiers, &hit.app, deps);
    }

    /// Takes the text waiting for the terminal's clipboard, and hands the same copy
    /// to every tab, so a middle click in another tab pastes it.
    pub fn take_copy(&mut self) -> Option<String> {
        let text = self.app_mut().take_copy()?;
        self.clipboard.clone_from(&text);
        for tab in &mut self.tabs {
            tab.app.set_clipboard(text.clone());
        }
        Some(text)
    }

    /// One pass of the loop: hooks, recaps, scroll measurement, and closes.
    pub fn poll(&mut self, deps: &Deps<'_>) -> Result<()> {
        for tab in &mut self.tabs {
            if let Some(rx) = tab.hooks.as_ref() {
                while let Ok(event) = rx.try_recv() {
                    tab.app.on_hook(&event);
                }
            }
            tab.app.poll_recap(deps);
        }
        self.app_mut().settle_scroll();
        if self.quitting {
            for tab in &mut self.tabs {
                tab.app.poll_close(deps)?;
            }
        }
        for app in &mut self.closing_tabs {
            app.poll_close(deps)?;
        }
        self.closing_tabs.retain(|app| !app.quit_requested());
        if self.quitting
            && self.closing_tabs.is_empty()
            && self.tabs.iter().all(|t| t.app.quit_requested())
        {
            for tab in &self.tabs {
                deps.hooks.release(&tab.app.project().slug);
            }
            self.quit = true;
        }
        Ok(())
    }

    /// Once a second, for every tab: reap a child that ended, and sample memory.
    pub fn on_tick(&mut self, probe: &dyn MemoryProbe, deps: &Deps<'_>) -> Result<()> {
        for tab in &mut self.tabs {
            tab.app.on_tick(probe, deps)?;
        }
        Ok(())
    }

    /// Sizes every tab's sessions to its own panel, the tabs not on screen included,
    /// so a switch never shows a stale layout.
    pub fn resize(&mut self, area: Rect) -> Result<()> {
        self.area = area;
        for index in 0..self.tabs.len() {
            let width = self.tabs[index].app.sidebar_width();
            let (rows, cols) = self.panel_size_for(width);
            self.tabs[index].app.resize_all(rows, cols)?;
        }
        Ok(())
    }

    /// Answers `Ctrl+q`: asks about the workspace when the open projects are worth
    /// keeping, and otherwise starts the quit at once.
    fn request_quit(&mut self, deps: &Deps<'_>) {
        if !needs_save_prompt(self.origin.as_ref(), &self.open_projects()) {
            self.begin_quit(deps);
            return;
        }
        let mut choices = Vec::new();
        if self.origin.is_some() {
            choices.push(SaveChoice::Update);
        }
        choices.push(SaveChoice::SaveAsNew);
        choices.push(SaveChoice::DontSave);
        self.modal = Some(BrowserModal::SavePrompt(SavePrompt {
            choices,
            cursor: 0,
            name: None,
            error: None,
        }));
    }

    /// Asks every tab to close. Every project saves its own state on the way out,
    /// whatever was decided about the workspace.
    fn begin_quit(&mut self, deps: &Deps<'_>) {
        self.modal = None;
        self.quitting = true;
        for tab in &mut self.tabs {
            tab.app.begin_close(deps);
        }
    }

    fn force_quit(&mut self) {
        for tab in &mut self.tabs {
            tab.app.force_close();
        }
        for app in &mut self.closing_tabs {
            app.force_close();
        }
    }

    /// Closes the focused tab. Its children leave in the background while the other
    /// tabs keep running. The last tab closes the window, prompt included.
    fn close_tab(&mut self, deps: &Deps<'_>) {
        if self.tabs.len() <= 1 {
            self.request_quit(deps);
            return;
        }
        let tab = self.tabs.remove(self.focused);
        deps.hooks.release(&tab.app.project().slug);
        let mut app = tab.app;
        app.begin_close(deps);
        self.closing_tabs.push(app);
        self.focused = self.focused.min(self.tabs.len().saturating_sub(1));
    }

    /// Opens the list of registered projects that no tab holds yet.
    fn open_picker(&mut self, deps: &Deps<'_>) {
        if self.tabs.len() >= MAX_TABS {
            self.app_mut().notify(format!(
                "{MAX_TABS} tabs is the limit. close one with Alt+Shift+W first."
            ));
            return;
        }
        let registry = match deps.store.load_registry() {
            Ok(registry) => registry,
            Err(e) => {
                self.app_mut()
                    .notify(format!("the project list did not load: {e}"));
                return;
            }
        };
        // A closed tab still leaving holds its project until its children are gone.
        let held: Vec<String> = self
            .tabs
            .iter()
            .map(|t| &t.app)
            .chain(self.closing_tabs.iter())
            .map(|app| app.project().slug.clone())
            .collect();
        let candidates = registry
            .projects
            .into_iter()
            .filter(|p| !held.contains(&p.slug))
            .collect();
        self.modal = Some(BrowserModal::Picker(Picker {
            candidates,
            ..Picker::default()
        }));
    }

    fn picker_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        let Some(BrowserModal::Picker(picker)) = self.modal.as_mut() else {
            return Ok(());
        };
        if host_action(key) == Some(HostAction::NewTab) {
            self.modal = None;
            return Ok(());
        }
        picker.notice = None;
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Down => picker.cursor = picker.cursor.saturating_add(1),
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.filter.push(c);
                picker.cursor = 0;
            }
            KeyCode::Enter => {
                let Some(entry) = picker.matches().get(picker.cursor).map(|e| (*e).clone()) else {
                    return Ok(());
                };
                return self.open_picked(&entry, deps);
            }
            _ => {}
        }
        if let Some(BrowserModal::Picker(picker)) = self.modal.as_mut() {
            let last = picker.matches().len().saturating_sub(1);
            picker.cursor = picker.cursor.min(last);
        }
        Ok(())
    }

    /// Opens the picked project as the last tab and focuses it. A project another
    /// culm holds stays out, and the picker says why.
    fn open_picked(&mut self, entry: &ProjectEntry, deps: &Deps<'_>) -> Result<()> {
        let project = deps
            .store
            .load_project(&entry.slug)?
            .unwrap_or_else(|| Project::new(&entry.slug, &entry.root));
        match self.add_tab(project, deps) {
            Ok(()) => {
                self.modal = None;
                self.focused = self.tabs.len() - 1;
            }
            Err(e) => {
                if let Some(BrowserModal::Picker(picker)) = self.modal.as_mut() {
                    picker.notice = Some(e.to_string());
                }
            }
        }
        Ok(())
    }

    fn save_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        let Some(BrowserModal::SavePrompt(prompt)) = self.modal.as_mut() else {
            return Ok(());
        };
        // A quit pressed again here would throw the answer away, so it waits.
        if host_action(key) == Some(HostAction::Quit) {
            return Ok(());
        }
        if let Some(name) = prompt.name.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    prompt.name = None;
                    prompt.error = None;
                }
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    name.push(c);
                }
                KeyCode::Enter => {
                    let name = name.clone();
                    match self.save_as_new(&name, deps) {
                        Ok(()) => self.begin_quit(deps),
                        Err(e) => {
                            if let Some(BrowserModal::SavePrompt(prompt)) = self.modal.as_mut() {
                                prompt.error = Some(e);
                            }
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        let last = prompt.choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
                prompt.cursor = prompt.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                prompt.cursor = (prompt.cursor + 1).min(last);
            }
            KeyCode::Enter => match prompt.choices.get(prompt.cursor).copied() {
                Some(SaveChoice::SaveAsNew) => {
                    prompt.name = Some(String::new());
                    prompt.error = None;
                }
                Some(SaveChoice::Update) => match self.save_update(deps) {
                    Ok(()) => self.begin_quit(deps),
                    Err(e) => {
                        if let Some(BrowserModal::SavePrompt(prompt)) = self.modal.as_mut() {
                            prompt.error = Some(e);
                        }
                    }
                },
                Some(SaveChoice::DontSave) => self.begin_quit(deps),
                None => {}
            },
            _ => {}
        }
        Ok(())
    }

    /// Writes the open projects as a new workspace. The error is a sentence for the
    /// prompt to show.
    fn save_as_new(&self, name: &str, deps: &Deps<'_>) -> std::result::Result<(), String> {
        let mut all = deps.store.load_workspaces().map_err(|e| e.to_string())?;
        all.create(name, &self.open_projects())
            .map_err(|e| e.to_string())?;
        deps.store.save_workspaces(&all).map_err(|e| e.to_string())
    }

    /// Writes the open projects into the workspace the window came from. A workspace
    /// deleted meanwhile is created again under its name.
    fn save_update(&self, deps: &Deps<'_>) -> std::result::Result<(), String> {
        let Some(origin) = self.origin.as_ref() else {
            return Ok(());
        };
        let mut all = deps.store.load_workspaces().map_err(|e| e.to_string())?;
        let projects = self.open_projects();
        if all.find(&origin.name).is_some() {
            all.set_projects(&origin.name, &projects)
                .map_err(|e| e.to_string())?;
        } else {
            all.create(&origin.name, &projects)
                .map_err(|e| e.to_string())?;
        }
        deps.store.save_workspaces(&all).map_err(|e| e.to_string())
    }
}
