//! A sortable, virtualized view of the current process sample.

use std::cmp::Ordering;

use eframe::egui::{self, Align2, AtomExt, FontId, RichText, Stroke, Ui, Vec2};

use crate::{
    processes::{Process, ProcessSnapshot},
    theme::*,
};

const ROW_HEIGHT: f32 = 32.0;
const CELL_PADDING: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Pid,
    User,
    Priority,
    Nice,
    Virtual,
    Resident,
    Shared,
    State,
    Cpu,
    Memory,
    CpuTime,
    Command,
}

impl Column {
    const ALL: [Self; 12] = [
        Self::Pid,
        Self::User,
        Self::Priority,
        Self::Nice,
        Self::Virtual,
        Self::Resident,
        Self::Shared,
        Self::State,
        Self::Cpu,
        Self::Memory,
        Self::CpuTime,
        Self::Command,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Pid => "PID",
            Self::User => "USER",
            Self::Priority => "PR",
            Self::Nice => "NI",
            Self::Virtual => "VIRT",
            Self::Resident => "RES",
            Self::Shared => "SHR",
            Self::State => "S",
            Self::Cpu => "CPU%",
            Self::Memory => "MEM%",
            Self::CpuTime => "TIME+",
            Self::Command => "COMMAND",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Pid => "Process ID",
            Self::User => "User",
            Self::Priority => "Scheduling priority; negative values indicate realtime priority",
            Self::Nice => "Nice value; lower values favor the process",
            Self::Virtual => "Virtual memory",
            Self::Resident => "Resident physical memory",
            Self::Shared => "Shared resident memory (approximate)",
            Self::State => "Process state",
            Self::Cpu => "CPU utilization; 100% equals one logical core",
            Self::Memory => "Resident memory as a percentage of physical RAM",
            Self::CpuTime => "Accumulated user and system CPU time",
            Self::Command => "Command line",
        }
    }

    fn width(self, ui: &Ui, processes: &[&Process]) -> f32 {
        let minimum: f32 = match self {
            Self::Pid => 82.0,
            Self::User => 80.0,
            Self::Priority | Self::Nice => 42.0,
            Self::Virtual | Self::Resident | Self::Shared => 94.0,
            Self::State => 46.0,
            Self::Cpu => 66.0,
            Self::Memory => 64.0,
            Self::CpuTime => 86.0,
            Self::Command => 210.0,
        };
        if !matches!(self, Self::Priority | Self::Cpu | Self::CpuTime) {
            return minimum;
        }
        // These numeric fields can outgrow their usual widths. Include offscreen
        // rows so scrolling never changes the column boundaries. All values use
        // the same monospace font, so only the longest string needs shaping.
        let longest = processes
            .iter()
            .map(|process| self.value(process))
            .max_by_key(String::len)
            .unwrap_or_default();
        let text_width = ui
            .painter()
            .layout_no_wrap(longest, FontId::monospace(13.0), TEXT)
            .size()
            .x;
        minimum.max((text_width + CELL_PADDING * 2.0 + 1.0).ceil())
    }

    fn initial_descending(self) -> bool {
        matches!(
            self,
            Self::Virtual
                | Self::Resident
                | Self::Shared
                | Self::Cpu
                | Self::Memory
                | Self::CpuTime
        )
    }

    fn value(self, process: &Process) -> String {
        match self {
            Self::Pid => process.pid.to_string(),
            Self::User => process.user.clone(),
            Self::Priority => process.priority.to_string(),
            Self::Nice => process.nice.to_string(),
            Self::Virtual => memory(process.virtual_bytes),
            Self::Resident => memory(process.resident_bytes),
            Self::Shared => memory(process.shared_bytes),
            Self::State => process.state.to_string(),
            Self::Cpu => percent(process.cpu_percent),
            Self::Memory => percent(process.memory_percent),
            Self::CpuTime => cpu_time(process.cpu_time_secs),
            Self::Command => command(process).to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StateFilter {
    #[default]
    All,
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    Zombie,
    Other,
}

impl StateFilter {
    const ALL: [Self; 7] = [
        Self::All,
        Self::Running,
        Self::Sleeping,
        Self::DiskSleep,
        Self::Stopped,
        Self::Zombie,
        Self::Other,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::All => "All states",
            Self::Running => "Running (R)",
            Self::Sleeping => "Sleeping / idle (S, I)",
            Self::DiskSleep => "Disk sleep (D)",
            Self::Stopped => "Stopped / tracing (T, t)",
            Self::Zombie => "Zombie (Z)",
            Self::Other => "Other states",
        }
    }

    fn matches(self, state: char) -> bool {
        self == Self::All || self == Self::for_state(state)
    }

    fn for_state(state: char) -> Self {
        match state {
            'R' => Self::Running,
            'S' | 'I' => Self::Sleeping,
            'D' => Self::DiskSleep,
            'T' | 't' => Self::Stopped,
            'Z' => Self::Zombie,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessKey {
    pid: u32,
    start_time_ticks: u64,
}

impl From<&Process> for ProcessKey {
    fn from(process: &Process) -> Self {
        Self {
            pid: process.pid,
            start_time_ticks: process.start_time_ticks,
        }
    }
}

pub struct ProcessView {
    query: String,
    state: StateFilter,
    sort: Column,
    descending: bool,
    selected: Option<ProcessKey>,
    focused: Option<(ProcessKey, egui::Id, usize)>,
}

impl Default for ProcessView {
    fn default() -> Self {
        Self {
            query: String::new(),
            state: StateFilter::All,
            sort: Column::Cpu,
            descending: true,
            selected: None,
            focused: None,
        }
    }
}

impl ProcessView {
    pub fn show(&mut self, ui: &mut Ui, snapshot: &ProcessSnapshot) {
        self.summary(ui, snapshot);
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(MANTLE)
            .stroke(Stroke::new(1.0, SURFACE0))
            .corner_radius(12)
            .inner_margin(14)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut filters_changed = false;
                ui.horizontal_wrapped(|ui| {
                    let search_label = ui.label("Search");
                    let search = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.query)
                                .id_salt("process_search")
                                .hint_text("PID, user, name, or command")
                                .desired_width(270.0),
                        )
                        .labelled_by(search_label.id);
                    filters_changed |= search.changed();
                    if search.gained_focus() {
                        search.scroll_to_me(None);
                    }
                    let state_label = ui.label("State");
                    let state = egui::ComboBox::from_id_salt("process_state")
                        .selected_text(self.state.title())
                        .show_ui(ui, |ui| {
                            for state in StateFilter::ALL {
                                filters_changed |= ui
                                    .selectable_value(&mut self.state, state, state.title())
                                    .changed();
                            }
                        })
                        .response
                        .labelled_by(state_label.id);
                    if state.gained_focus() {
                        state.scroll_to_me(None);
                    }
                    if !self.query.is_empty() || self.state != StateFilter::All {
                        let clear = ui.button("Clear filters");
                        if clear.gained_focus() {
                            clear.scroll_to_me(None);
                        }
                        if clear.clicked() {
                            self.query.clear();
                            self.state = StateFilter::All;
                            filters_changed = true;
                        }
                    }
                });

                let mut processes = filtered_processes(snapshot, &self.query, self.state);
                processes.sort_unstable_by(|left, right| {
                    compare_processes(left, right, self.sort, self.descending)
                });
                ui.horizontal_wrapped(|ui| {
                    secondary(
                        ui,
                        format!(
                            "{} of {} processes · Sorted by {} {}",
                            processes.len(),
                            snapshot.processes.len(),
                            self.sort.title(),
                            if self.descending { "descending" } else { "ascending" }
                        ),
                    );
                    secondary(ui, "Select a row for details. Use Up / Down, Page Up / Down, Home / End to navigate.");
                });
                secondary(
                    ui,
                    "CPU: 100% = one logical core; multithreaded processes may exceed 100%. MEM: share of physical RAM.",
                );
                ui.add_space(4.0);

                if processes.is_empty() {
                    ui.add_space(16.0);
                    ui.label(if snapshot.processes.is_empty() {
                        "No processes are available in the current sample."
                    } else {
                        "No processes match these filters."
                    });
                    ui.add_space(16.0);
                } else {
                    self.table(ui, &processes, filters_changed);
                }

                ui.add_space(8.0);
                secondary(
                    ui,
                    "Memory uses binary units. TIME+ is accumulated CPU time. — means unavailable or awaiting a second sample.",
                );
                if !snapshot.warnings.is_empty() {
                    let notices = egui::CollapsingHeader::new(
                        RichText::new(format!(
                            "{} process availability notices",
                            snapshot.warnings.len()
                        ))
                        .color(YELLOW),
                    )
                    .id_salt("process_notices")
                    .show(ui, |ui| {
                        for warning in &snapshot.warnings {
                            ui.label(warning);
                        }
                    });
                    if notices.header_response.gained_focus() {
                        notices.header_response.scroll_to_me(None);
                    }
                }
            });
        ui.add_space(12.0);
        self.details(ui, snapshot);
    }

    fn summary(&self, ui: &mut Ui, snapshot: &ProcessSnapshot) {
        let counts = |state: StateFilter| {
            snapshot
                .processes
                .iter()
                .filter(|process| state.matches(process.state))
                .count()
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} processes", snapshot.processes.len()))
                    .size(21.0)
                    .strong()
                    .color(BLUE),
            );
            for (state, label, color) in [
                (StateFilter::Running, "running", GREEN),
                (StateFilter::Sleeping, "sleeping / idle", SUBTEXT),
                (StateFilter::DiskSleep, "disk sleep", YELLOW),
                (StateFilter::Stopped, "stopped", PEACH),
                (StateFilter::Zombie, "zombie", RED),
            ] {
                ui.label(RichText::new(format!("{} {label}", counts(state))).color(color));
            }
            let other = counts(StateFilter::Other);
            if other > 0 {
                secondary(ui, format!("{other} other"));
            }
        });
    }

    fn table(&mut self, ui: &mut Ui, processes: &[&Process], filters_changed: bool) {
        // Only visible rows are instantiated, and their identities follow the process,
        // not the current sort position. Both axes retain native scrollbars.
        let mut columns = Column::ALL.map(|column| (column, column.width(ui, processes)));
        let minimum_width: f32 = columns.iter().map(|(_, width)| width).sum();
        let table_width = ui.available_width().max(minimum_width);
        for (column, width) in &mut columns {
            if *column == Column::Command {
                *width += table_width - minimum_width;
            }
        }
        let viewport_height = ui.ctx().content_rect().height();
        let table_height = (viewport_height - 570.0).clamp(240.0, 430.0);
        let row_stride = ROW_HEIGHT + 2.0;
        let mut focus_target = None;
        let mut scroll_target = None;
        let mut keyboard_navigation = false;
        let mut reveal_in_page = None;
        if let Some((key, id, previous_index)) = self.focused
            && ui.memory(|memory| memory.has_focus(id))
            && let Some(index) = processes
                .iter()
                .position(|process| ProcessKey::from(*process) == key)
        {
            let page_rows = (table_height / row_stride).floor() as usize;
            let next = ui.input_mut(|input| {
                [
                    egui::Key::ArrowUp,
                    egui::Key::ArrowDown,
                    egui::Key::PageUp,
                    egui::Key::PageDown,
                    egui::Key::Home,
                    egui::Key::End,
                ]
                .into_iter()
                .find(|key| input.consume_key(egui::Modifiers::NONE, *key))
                .map(|key| navigation_target(index, processes.len(), page_rows, key))
            });
            if let Some(next) = next {
                keyboard_navigation = true;
                let key = ProcessKey::from(processes[next]);
                self.selected = Some(key);
                focus_target = Some(key);
                scroll_target = Some(next);
            } else if index != previous_index {
                // A live sort can move the focused process outside the instantiated
                // rows. Follow that identity so it does not lose keyboard focus.
                focus_target = Some(key);
                scroll_target = Some(index);
            }
        }
        egui::ScrollArea::horizontal()
            .id_salt("process_horizontal_scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_width(table_width);
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                    ui.spacing_mut().button_padding = Vec2::new(CELL_PADDING, 4.0);
                    ui.spacing_mut().interact_size.y = ROW_HEIGHT;
                    ui.horizontal(|ui| {
                        for &(column, width) in &columns {
                            let active = self.sort == column;
                            let arrow = if active {
                                if self.descending { " ↓" } else { " ↑" }
                            } else {
                                ""
                            };
                            let response = ui.add_sized(
                                [width, ROW_HEIGHT],
                                egui::Button::new(
                                    RichText::new(format!("{}{arrow}", column.title()))
                                        .font(FontId::monospace(12.0))
                                        .color(if active { BLUE } else { SUBTEXT }),
                                )
                                .selected(active)
                                .fill(if active { SURFACE0 } else { MANTLE })
                                .stroke(Stroke::NONE)
                                .corner_radius(4),
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    ui.is_enabled(),
                                    active,
                                    format!(
                                        "Sort by {}{}",
                                        column.description(),
                                        if active {
                                            if self.descending {
                                                "; currently descending"
                                            } else {
                                                "; currently ascending"
                                            }
                                        } else {
                                            ""
                                        }
                                    ),
                                )
                            });
                            if response.gained_focus() {
                                response.scroll_to_me(None);
                                reveal_in_page = Some(response.rect);
                            }
                            focus_border(ui, &response);
                            if response.clicked() {
                                if active {
                                    self.descending = !self.descending;
                                } else {
                                    self.sort = column;
                                    self.descending = column.initial_descending();
                                }
                            }
                            response.on_hover_text(column.description());
                        }
                    });
                    ui.separator();
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("process_rows")
                        .max_height(table_height)
                        .min_scrolled_height(96.0)
                        .auto_shrink([false, true]);
                    if filters_changed {
                        scroll = scroll.vertical_scroll_offset(0.0);
                    } else if let Some(index) = scroll_target {
                        scroll = scroll.vertical_scroll_offset(
                            (index as f32 * row_stride - table_height * 0.5).max(0.0),
                        );
                    }
                    let mut reveal_row = None;
                    let rows = scroll.show_rows(ui, ROW_HEIGHT, processes.len(), |ui, range| {
                        for row in range {
                            let process = processes[row];
                            let key = ProcessKey::from(process);
                            ui.scope_builder(
                                egui::UiBuilder::new().id(egui::Id::new((
                                    "process_row",
                                    key.pid,
                                    key.start_time_ticks,
                                ))),
                                |ui| {
                                    let mut atoms = egui::Atoms::new(());
                                    for &(column, width) in &columns {
                                        let width = width - CELL_PADDING * 2.0;
                                        let align = match column {
                                            Column::User | Column::Command => Align2::LEFT_CENTER,
                                            Column::State => Align2::CENTER_CENTER,
                                            _ => Align2::RIGHT_CENTER,
                                        };
                                        let color = if column == Column::Cpu {
                                            BLUE
                                        } else if column == Column::State && process.state == 'Z' {
                                            RED
                                        } else {
                                            TEXT
                                        };
                                        atoms.push_right(
                                            RichText::new(column.value(process))
                                                .font(FontId::monospace(13.0))
                                                .color(color)
                                                .atom_size(Vec2::new(width, 20.0))
                                                .atom_max_width(width)
                                                .atom_align(align),
                                        );
                                    }
                                    let selected = self.selected == Some(key);
                                    let response = ui.add_sized(
                                        [table_width, ROW_HEIGHT],
                                        egui::Button::new(atoms)
                                            .gap(CELL_PADDING * 2.0)
                                            .selected(selected)
                                            .fill(if selected {
                                                SURFACE0
                                            } else if row % 2 == 0 {
                                                BASE
                                            } else {
                                                MANTLE
                                            })
                                            .stroke(if selected {
                                                Stroke::new(1.0, MAUVE)
                                            } else {
                                                Stroke::NONE
                                            })
                                            .corner_radius(4),
                                    );
                                    response.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Button,
                                            ui.is_enabled(),
                                            selected,
                                            accessible_row(process),
                                        )
                                    });
                                    if response.clicked() {
                                        self.selected = Some(key);
                                    }
                                    if response.clicked() || focus_target == Some(key) {
                                        response.request_focus();
                                    }
                                    if response.gained_focus()
                                        || (keyboard_navigation && focus_target == Some(key))
                                    {
                                        response.scroll_to_me(None);
                                        reveal_row = Some(response.rect);
                                    }
                                    if response.has_focus() {
                                        self.focused = Some((key, response.id, row));
                                        ui.memory_mut(|memory| {
                                            memory.set_focus_lock_filter(
                                                response.id,
                                                egui::EventFilter {
                                                    vertical_arrows: true,
                                                    ..Default::default()
                                                },
                                            );
                                        });
                                    }
                                    focus_border(ui, &response);
                                },
                            );
                        }
                    });
                    if let Some(mut rect) = reveal_row {
                        // The inner scroll reveals the row at its nearest edge.
                        // Reveal that position in the page without also moving
                        // the user's horizontal position in this wide table.
                        let height = rect.height().min(rows.inner_rect.height());
                        rect.min.y = rect
                            .top()
                            .clamp(rows.inner_rect.top(), rows.inner_rect.bottom() - height);
                        rect.max.y = rect.min.y + height;
                        reveal_in_page = Some(rect);
                    }
                });
            });
        // Each ScrollArea consumes scroll requests, including axes it does not
        // scroll. Forward focus visibility after both table scroll areas close.
        if let Some(rect) = reveal_in_page {
            ui.scroll_to_rect(rect, None);
        }
    }

    fn details(&mut self, ui: &mut Ui, snapshot: &ProcessSnapshot) {
        let Some(key) = self.selected else {
            return;
        };
        egui::Frame::new()
            .fill(MANTLE)
            .stroke(Stroke::new(1.0, SURFACE0))
            .corner_radius(12)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("Process details · PID {}", key.pid))
                            .size(16.0)
                            .strong()
                            .color(BLUE),
                    );
                    let clear = ui.button("Clear selection");
                    if clear.gained_focus() {
                        clear.scroll_to_me(None);
                    }
                    if clear.clicked() {
                        self.selected = None;
                    }
                });
                let Some(process) = selected_process(snapshot, key) else {
                    ui.label("This process is no longer available in the current sample.");
                    secondary(
                        ui,
                        "It may have exited or become unreadable. Select another process to inspect it.",
                    );
                    return;
                };
                ui.add_space(4.0);
                ui.label(format!("Name: {}", process.name));
                ui.label(format!("Command: {}", command(process)));
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "User: {} · UID: {}",
                        process.user,
                        process.uid.map_or_else(|| "—".into(), |uid| uid.to_string())
                    ));
                    ui.label(format!("Parent PID: {}", process.ppid));
                    ui.label(format!(
                        "State: {} ({})",
                        state_name(process.state),
                        process.state
                    ));
                    ui.label(format!("Threads: {}", process.threads));
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Priority: {}", process.priority));
                    ui.label(format!("Nice: {}", process.nice));
                    ui.label(format!("CPU: {}%", percent(process.cpu_percent)));
                    ui.label(format!("Memory: {}% of RAM", percent(process.memory_percent)));
                });
                ui.horizontal_wrapped(|ui| {
                    let duration = if process.cpu_time_secs.is_finite() && process.cpu_time_secs >= 0.0 {
                        format!("{} ({:.2} seconds)", cpu_time(process.cpu_time_secs), process.cpu_time_secs)
                    } else {
                        "—".into()
                    };
                    ui.label(format!("CPU time: {duration}"));
                    ui.label(format!(
                        "Elapsed: {}",
                        process.elapsed_secs.map_or_else(|| "—".into(), elapsed_time)
                    ));
                });
                for (label, bytes) in [
                    ("Virtual memory", process.virtual_bytes),
                    ("Resident memory", process.resident_bytes),
                    ("Shared resident memory (approximate)", process.shared_bytes),
                ] {
                    ui.label(format!("{label}: {}", exact_memory(bytes)));
                }
            });
    }
}

fn navigation_target(current: usize, length: usize, page_rows: usize, key: egui::Key) -> usize {
    let last = length.saturating_sub(1);
    match key {
        egui::Key::ArrowUp => current.saturating_sub(1),
        egui::Key::ArrowDown => current.saturating_add(1).min(last),
        egui::Key::PageUp => current.saturating_sub(page_rows.max(1)),
        egui::Key::PageDown => current.saturating_add(page_rows.max(1)).min(last),
        egui::Key::Home => 0,
        egui::Key::End => last,
        _ => current.min(last),
    }
}

fn selected_process(snapshot: &ProcessSnapshot, key: ProcessKey) -> Option<&Process> {
    snapshot
        .processes
        .iter()
        .find(|process| ProcessKey::from(*process) == key)
}

fn filtered_processes<'a>(
    snapshot: &'a ProcessSnapshot,
    query: &str,
    state: StateFilter,
) -> Vec<&'a Process> {
    let query = query.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    snapshot
        .processes
        .iter()
        .filter(|process| {
            if !state.matches(process.state) {
                return false;
            }
            if terms.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {}",
                process.pid, process.user, process.name, process.command
            )
            .to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}

fn compare_processes(
    left: &Process,
    right: &Process,
    column: Column,
    descending: bool,
) -> Ordering {
    let ordered = |ordering: Ordering| {
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    };
    let comparison = match column {
        Column::Pid => ordered(left.pid.cmp(&right.pid)),
        Column::User => ordered(left.user.cmp(&right.user)),
        Column::Priority => ordered(left.priority.cmp(&right.priority)),
        Column::Nice => ordered(left.nice.cmp(&right.nice)),
        Column::Virtual => optional_cmp(left.virtual_bytes, right.virtual_bytes, descending),
        Column::Resident => optional_cmp(left.resident_bytes, right.resident_bytes, descending),
        Column::Shared => optional_cmp(left.shared_bytes, right.shared_bytes, descending),
        Column::State => ordered(left.state.cmp(&right.state)),
        Column::Cpu => optional_cmp(
            finite(left.cpu_percent),
            finite(right.cpu_percent),
            descending,
        ),
        Column::Memory => optional_cmp(
            finite(left.memory_percent),
            finite(right.memory_percent),
            descending,
        ),
        Column::CpuTime => optional_cmp(
            finite(Some(left.cpu_time_secs)),
            finite(Some(right.cpu_time_secs)),
            descending,
        ),
        Column::Command => ordered(command(left).cmp(command(right))),
    };
    // Keep ties steady between refreshes, regardless of the selected direction.
    comparison.then_with(|| left.pid.cmp(&right.pid))
}

fn optional_cmp<T: PartialOrd>(left: Option<T>, right: Option<T>, descending: bool) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.partial_cmp(&right).unwrap_or(Ordering::Equal);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

fn command(process: &Process) -> &str {
    if process.command.is_empty() {
        &process.name
    } else {
        &process.command
    }
}

fn percent(value: Option<f64>) -> String {
    finite(value).map_or_else(|| "—".into(), |value| format!("{value:.1}"))
}

fn memory(value: Option<u64>) -> String {
    let Some(bytes) = value else {
        return "—".into();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        value /= 1024.0;
        if value < 1024.0 || unit == "EiB" {
            return format!("{value:.1} {unit}");
        }
    }
    unreachable!()
}

fn exact_memory(value: Option<u64>) -> String {
    value.map_or_else(
        || "—".into(),
        |bytes| format!("{} ({bytes} bytes)", memory(value)),
    )
}

fn cpu_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".into();
    }
    let hundredths = (seconds * 100.0).round() as u64;
    let whole = hundredths / 100;
    if whole >= 3600 {
        format!("{}:{:02}:{:02}", whole / 3600, whole / 60 % 60, whole % 60)
    } else {
        format!("{}:{:02}.{:02}", whole / 60, whole % 60, hundredths % 100)
    }
}

fn elapsed_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".into();
    }
    let seconds = seconds as u64;
    format!(
        "{}d {:02}h {:02}m {:02}s",
        seconds / 86400,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn state_name(state: char) -> &'static str {
    match state {
        'R' => "Running / runnable",
        'S' => "Sleeping",
        'D' => "Uninterruptible disk sleep",
        'T' => "Stopped",
        't' => "Tracing stop",
        'Z' => "Zombie",
        'I' => "Idle",
        'X' | 'x' => "Dead",
        'W' => "Paging / waking",
        'P' => "Parked",
        _ => "Other",
    }
}

fn accessible_row(process: &Process) -> String {
    format!(
        "PID {}, user {}, priority {}, nice {}, virtual {}, resident {}, shared {}, state {} ({}), CPU {} percent, memory {} percent, CPU time {}, command {}. Select for process details.",
        process.pid,
        process.user,
        process.priority,
        process.nice,
        memory(process.virtual_bytes),
        memory(process.resident_bytes),
        memory(process.shared_bytes),
        state_name(process.state),
        process.state,
        percent(process.cpu_percent),
        percent(process.memory_percent),
        cpu_time(process.cpu_time_secs),
        command(process),
    )
}

fn secondary(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).size(12.0).color(SUBTEXT));
}

fn focus_border(ui: &Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            4,
            Stroke::new(2.0, LAVENDER),
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, cpu: Option<f64>) -> Process {
        Process {
            pid,
            ppid: 1,
            uid: Some(1000),
            user: "alice".into(),
            state: 'S',
            priority: 20,
            nice: 0,
            threads: 1,
            virtual_bytes: Some(2048),
            resident_bytes: Some(1024),
            shared_bytes: Some(512),
            cpu_percent: cpu,
            memory_percent: Some(1.0),
            cpu_time_secs: 125.12,
            elapsed_secs: Some(900.0),
            name: "worker".into(),
            command: "/usr/bin/worker --serve".into(),
            start_time_ticks: 100,
        }
    }

    #[test]
    fn cpu_sort_is_numeric_and_missing_stays_last_in_both_directions() {
        let mut processes = [
            process(4, None),
            process(3, Some(9.0)),
            process(2, Some(120.0)),
            process(1, Some(9.0)),
        ];
        processes.sort_by(|a, b| compare_processes(a, b, Column::Cpu, true));
        assert_eq!(processes.map(|process| process.pid), [2, 1, 3, 4]);
        let mut processes = [
            process(4, None),
            process(3, Some(9.0)),
            process(2, Some(120.0)),
            process(1, Some(9.0)),
        ];
        processes.sort_by(|a, b| compare_processes(a, b, Column::Cpu, false));
        assert_eq!(processes.map(|process| process.pid), [1, 3, 2, 4]);
    }

    #[test]
    fn memory_sort_uses_bytes_and_handles_missing_and_nonfinite_values() {
        let mut small = process(1, Some(f64::NAN));
        let mut large = process(2, Some(0.0));
        small.resident_bytes = Some(900);
        large.resident_bytes = Some(1024 * 1024);
        assert_eq!(
            compare_processes(&small, &large, Column::Resident, false),
            Ordering::Less
        );
        assert_eq!(
            compare_processes(&small, &large, Column::Cpu, true),
            Ordering::Greater
        );
        small.resident_bytes = None;
        for descending in [false, true] {
            assert_eq!(
                compare_processes(&small, &large, Column::Resident, descending),
                Ordering::Greater
            );
        }
    }

    #[test]
    fn all_equal_column_values_use_ascending_pid_tiebreak() {
        let first = process(1, Some(9.0));
        let second = process(2, Some(9.0));
        for column in Column::ALL
            .into_iter()
            .filter(|column| *column != Column::Pid)
        {
            for descending in [false, true] {
                assert_eq!(
                    compare_processes(&first, &second, column, descending),
                    Ordering::Less
                );
            }
        }
    }

    #[test]
    fn filtering_matches_pid_user_name_and_command_case_insensitively() {
        let mut other = process(912, Some(0.0));
        other.user = "bob".into();
        other.name = "database".into();
        other.command = "/usr/bin/postgres --port=5432".into();
        other.state = 'R';
        let snapshot = ProcessSnapshot {
            processes: vec![process(123, Some(0.0)), other],
            warnings: vec![],
        };
        for query in ["912", "BOB", "DATABASE", "5432", "  bob  POSTGRES "] {
            let filtered = filtered_processes(&snapshot, query, StateFilter::All);
            assert_eq!(
                filtered
                    .iter()
                    .map(|process| process.pid)
                    .collect::<Vec<_>>(),
                vec![912]
            );
        }
        assert!(filtered_processes(&snapshot, "bob", StateFilter::Sleeping).is_empty());
        assert_eq!(
            filtered_processes(&snapshot, "  ", StateFilter::All).len(),
            2
        );
        assert_eq!(
            filtered_processes(&snapshot, "", StateFilter::Running)[0].pid,
            912
        );
    }

    #[test]
    fn state_filter_classifies_idle_and_tracing_without_losing_other_states() {
        assert!(StateFilter::Sleeping.matches('I'));
        assert!(StateFilter::Stopped.matches('t'));
        assert!(StateFilter::Other.matches('X'));
        assert!(!StateFilter::Sleeping.matches('D'));
        for state in ['R', 'S', 'D', 'T', 't', 'Z', 'I', 'X', '?'] {
            assert_eq!(
                StateFilter::ALL
                    .into_iter()
                    .filter(|filter| *filter != StateFilter::All && filter.matches(state))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn selection_never_attaches_to_a_reused_pid() {
        let original = process(123, Some(0.0));
        let key = ProcessKey::from(&original);
        let mut replacement = original.clone();
        replacement.start_time_ticks += 1;
        let mut snapshot = ProcessSnapshot {
            processes: vec![replacement],
            warnings: vec![],
        };
        assert!(selected_process(&snapshot, key).is_none());
        snapshot.processes.clear();
        assert!(selected_process(&snapshot, key).is_none());
        snapshot.processes.push(original);
        assert!(selected_process(&snapshot, key).is_some());
    }

    #[test]
    fn keyboard_navigation_reaches_offscreen_rows_and_clamps_at_both_ends() {
        assert_eq!(navigation_target(0, 1000, 12, egui::Key::ArrowUp), 0);
        assert_eq!(navigation_target(999, 1000, 12, egui::Key::ArrowDown), 999);
        assert_eq!(navigation_target(500, 1000, 12, egui::Key::PageDown), 512);
        assert_eq!(navigation_target(5, 1000, 12, egui::Key::PageUp), 0);
        assert_eq!(navigation_target(5, 1000, 12, egui::Key::End), 999);
        assert_eq!(navigation_target(999, 1000, 12, egui::Key::Home), 0);
    }

    #[test]
    fn keyboard_focus_reveals_filters_headers_rows_and_details() {
        for size in [egui::vec2(640.0, 480.0), egui::vec2(320.0, 240.0)] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.all_styles_mut(|style| {
                style.scroll_animation = egui::style::ScrollAnimation::none();
            });
            let mut view = ProcessView {
                query: "worker".into(),
                ..Default::default()
            };
            let mut snapshot = ProcessSnapshot {
                processes: (1..=100).map(|pid| process(pid, Some(0.0))).collect(),
                warnings: vec!["Some process details are unavailable.".into()],
            };
            let key_event = |key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let frame = |view: &mut ProcessView, snapshot: &ProcessSnapshot, events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("process_test_page")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                // Model the app header above the process view.
                                ui.add_space(150.0);
                                view.show(ui, snapshot);
                            });
                    },
                );
                output.drop_without_applying_deltas();
            };
            frame(&mut view, &snapshot, vec![]);
            frame(&mut view, &snapshot, vec![]);
            // Search, state, Clear filters, all twelve headers, and several rows.
            for tab in 0..23 {
                frame(&mut view, &snapshot, vec![key_event(egui::Key::Tab)]);
                for _ in 0..4 {
                    frame(&mut view, &snapshot, vec![]);
                }
                let response = ctx
                    .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                    .unwrap();
                assert!(
                    response.interact_rect.top() <= response.rect.top() + 1.0
                        && response.interact_rect.bottom() >= response.rect.bottom() - 1.0,
                    "{size:?}, tab {tab}: focused {:?}, visible {:?}",
                    response.rect,
                    response.interact_rect,
                );
                if tab < 15 {
                    assert!(
                        response.interact_rect.left() <= response.rect.left() + 1.0
                            && response.interact_rect.right() >= response.rect.right() - 1.0,
                        "{size:?}, tab {tab}: focused {:?}, visible {:?}",
                        response.rect,
                        response.interact_rect,
                    );
                }
            }
            let horizontal_position = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap()
                .rect
                .left();
            for key in [egui::Key::End, egui::Key::Home, egui::Key::PageDown] {
                frame(&mut view, &snapshot, vec![key_event(key)]);
                for _ in 0..4 {
                    frame(&mut view, &snapshot, vec![]);
                }
                let response = ctx
                    .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                    .unwrap();
                assert!(
                    response.interact_rect.top() <= response.rect.top() + 1.0
                        && response.interact_rect.bottom() >= response.rect.bottom() - 1.0,
                    "{size:?}, {key:?}: focused {:?}, visible {:?}",
                    response.rect,
                    response.interact_rect,
                );
                assert_eq!(response.rect.left(), horizontal_position);
            }
            // An intentional horizontal wheel scroll must survive later frames,
            // keyboard movement, and a live CPU sort that moves the same process.
            let response = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap();
            frame(
                &mut view,
                &snapshot,
                vec![
                    egui::Event::PointerMoved(response.interact_rect.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(100.0, 0.0),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            for _ in 0..20 {
                frame(&mut view, &snapshot, vec![]);
            }
            let scrolled = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap()
                .rect
                .left();
            assert!(scrolled > horizontal_position + 50.0);
            let selected = view.selected.unwrap();
            snapshot
                .processes
                .iter_mut()
                .find(|process| ProcessKey::from(&**process) == selected)
                .unwrap()
                .cpu_percent = Some(100.0);
            for _ in 0..4 {
                frame(&mut view, &snapshot, vec![]);
            }
            assert_eq!(view.focused.unwrap().0, selected);
            assert_eq!(view.focused.unwrap().2, 0);
            frame(&mut view, &snapshot, vec![key_event(egui::Key::ArrowDown)]);
            for _ in 0..4 {
                frame(&mut view, &snapshot, vec![]);
            }
            let response = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap();
            assert_eq!(response.rect.left(), scrolled);

            // From the final process row, Tab reaches the notices header and
            // then Clear selection. Both must reveal themselves in the page.
            frame(&mut view, &snapshot, vec![key_event(egui::Key::End)]);
            for _ in 0..4 {
                frame(&mut view, &snapshot, vec![]);
            }
            frame(&mut view, &snapshot, vec![key_event(egui::Key::Tab)]);
            for _ in 0..4 {
                frame(&mut view, &snapshot, vec![]);
            }
            let notices = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap();
            assert_ne!(notices.id, view.focused.unwrap().1);
            assert!(
                notices.interact_rect.top() <= notices.rect.top() + 1.0
                    && notices.interact_rect.bottom() >= notices.rect.bottom() - 1.0,
                "{size:?}, Process notices: focused {:?}, visible {:?}",
                notices.rect,
                notices.interact_rect,
            );
            frame(&mut view, &snapshot, vec![key_event(egui::Key::Tab)]);
            for _ in 0..4 {
                frame(&mut view, &snapshot, vec![]);
            }
            let response = ctx
                .read_response(ctx.memory(|memory| memory.focused()).unwrap())
                .unwrap();
            assert_ne!(response.id, view.focused.unwrap().1);
            assert_ne!(response.id, notices.id);
            assert!(
                response.interact_rect.top() <= response.rect.top() + 1.0
                    && response.interact_rect.bottom() >= response.rect.bottom() - 1.0,
                "{size:?}, Clear selection: focused {:?}, visible {:?}",
                response.rect,
                response.interact_rect,
            );

            // Revealing a newly focused control must not continuously pull
            // the page back when the user then chooses to scroll manually.
            frame(
                &mut view,
                &snapshot,
                vec![
                    egui::Event::PointerMoved(response.interact_rect.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -100.0),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            for _ in 0..20 {
                frame(&mut view, &snapshot, vec![]);
            }
            let manually_scrolled = ctx.read_response(response.id).unwrap();
            assert!(manually_scrolled.has_focus());
            assert!(manually_scrolled.rect.top() < response.rect.top() - 50.0);
        }
    }

    #[test]
    fn memory_cells_keep_numbers_and_units_intact_at_supported_sizes() {
        let expected = [
            "1023.0 KiB",
            "1023.0 MiB",
            "1023.0 GiB",
            "1024.0 KiB",
            "1024.0 MiB",
            "1024.0 GiB",
        ];
        let processes: Vec<_> = [false, true]
            .into_iter()
            .enumerate()
            .map(|(index, rounded_boundary)| {
                let mut process = process(index as u32 + 1, Some(0.0));
                let values = [1024_u64, 1024_u64.pow(2), 1024_u64.pow(3)].map(|unit| {
                    Some(if rounded_boundary {
                        1024 * unit - 1
                    } else {
                        1023 * unit
                    })
                });
                [
                    process.virtual_bytes,
                    process.resident_bytes,
                    process.shared_bytes,
                ] = values;
                process
            })
            .collect();
        let rows: Vec<_> = processes.iter().collect();
        for size in [
            egui::vec2(320.0, 240.0),
            egui::vec2(640.0, 480.0),
            egui::vec2(1440.0, 1000.0),
        ] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.all_styles_mut(|style| {
                style.scroll_animation = egui::style::ScrollAnimation::none();
            });
            let mut view = ProcessView::default();
            let mut seen = [false; 6];
            for frame in 0..40 {
                // Focusing successive headers reveals each column when the
                // table is wider than the viewport, just as in the process page.
                let events = if frame >= 4 && frame % 4 == 0 {
                    vec![egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]
                } else {
                    vec![]
                };
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ui| view.table(ui, &rows, false),
                );
                for clipped in &output.shapes {
                    let egui::Shape::Text(text) = &clipped.shape else {
                        continue;
                    };
                    let Some(index) = expected
                        .iter()
                        .position(|value| *value == text.galley.job.text)
                    else {
                        continue;
                    };
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    if !clipped.clip_rect.contains_rect(rect) {
                        continue;
                    }
                    assert!(
                        !text.galley.elided,
                        "{size:?}: {} is truncated",
                        expected[index]
                    );
                    assert_eq!(text.galley.rows.len(), 1);
                    let painted: String = text.galley.rows[0]
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.chr)
                        .collect();
                    assert_eq!(painted, expected[index], "{size:?}: memory text changed");
                    seen[index] = true;
                }
                output.drop_without_applying_deltas();
                if seen.iter().all(|visible| *visible) {
                    break;
                }
            }
            assert_eq!(seen, [true; 6], "{size:?}: memory cells were unreachable");
        }
    }

    #[test]
    fn numeric_cells_keep_complete_values_at_supported_sizes() {
        let expected = ["-100", "12800.0", "1000:00:00"];
        let mut process = process(1, Some(12800.0));
        process.priority = -100;
        process.cpu_time_secs = 1000.0 * 3600.0;
        let rows = [&process];
        for size in [
            egui::vec2(320.0, 240.0),
            egui::vec2(640.0, 480.0),
            egui::vec2(1440.0, 1000.0),
        ] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.all_styles_mut(|style| {
                style.scroll_animation = egui::style::ScrollAnimation::none();
            });
            let mut view = ProcessView::default();
            let mut seen = [false; 3];
            for frame in 0..48 {
                // Tab through the headers to reveal numeric columns in narrow windows.
                let events = if frame >= 4 && frame % 4 == 0 {
                    vec![egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]
                } else {
                    vec![]
                };
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ui| view.table(ui, &rows, false),
                );
                // Release renderer deltas before assertions so failures unwind cleanly.
                output.textures_delta.clear();
                for clipped in &output.shapes {
                    let egui::Shape::Text(text) = &clipped.shape else {
                        continue;
                    };
                    let Some(index) = expected
                        .iter()
                        .position(|value| *value == text.galley.job.text)
                    else {
                        continue;
                    };
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    if !clipped.clip_rect.contains_rect(rect) {
                        continue;
                    }
                    assert!(
                        !text.galley.elided,
                        "{size:?}: {} is truncated",
                        expected[index]
                    );
                    assert_eq!(text.galley.rows.len(), 1);
                    let painted: String = text.galley.rows[0]
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.chr)
                        .collect();
                    assert_eq!(painted, expected[index], "{size:?}: numeric text changed");
                    seen[index] = true;
                }
                output.drop_without_applying_deltas();
                if seen.iter().all(|visible| *visible) {
                    break;
                }
            }
            assert_eq!(seen, [true; 3], "{size:?}: numeric cells were unreachable");
        }
    }

    #[test]
    fn numeric_column_widths_account_for_initially_virtualized_rows() {
        let mut processes: Vec<_> = (1..=100).map(|pid| process(pid, Some(0.0))).collect();
        let last = processes.last_mut().unwrap();
        last.priority = -100;
        last.cpu_percent = Some(12800.0);
        last.cpu_time_secs = 1000.0 * 3600.0;
        let rows: Vec<_> = processes.iter().collect();
        for size in [
            egui::vec2(320.0, 240.0),
            egui::vec2(640.0, 480.0),
            egui::vec2(1440.0, 1000.0),
        ] {
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            ctx.all_styles_mut(|style| {
                style.scroll_animation = egui::style::ScrollAnimation::none();
            });
            let mut view = ProcessView::default();
            let frame = |view: &mut ProcessView, key: Option<egui::Key>| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events: key
                            .into_iter()
                            .map(|key| egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            })
                            .collect(),
                        ..Default::default()
                    },
                    |ui| view.table(ui, &rows, false),
                );
                output.drop_without_applying_deltas();
            };
            for _ in 0..4 {
                frame(&mut view, None);
            }
            let mut headers = vec![];
            // Visit all headers, then the first process row.
            for tab in 0..13 {
                frame(&mut view, Some(egui::Key::Tab));
                for _ in 0..4 {
                    frame(&mut view, None);
                }
                if [2, 8, 10].contains(&tab) {
                    let id = ctx.memory(|memory| memory.focused()).unwrap();
                    headers.push((id, ctx.read_response(id).unwrap().rect.width()));
                }
            }
            assert_eq!(view.focused.unwrap().2, 0);
            frame(&mut view, Some(egui::Key::End));
            for _ in 0..4 {
                frame(&mut view, None);
            }
            assert_eq!(view.focused.unwrap().2, 99);
            for (id, initial_width) in headers {
                let width = ctx.read_response(id).unwrap().rect.width();
                assert!(
                    (width - initial_width).abs() < 0.01,
                    "{size:?}: numeric column width changed from {initial_width} to {width} when a long value became visible",
                );
            }
        }
    }

    #[test]
    fn time_and_memory_values_preserve_units_and_boundary_rollover() {
        assert_eq!(cpu_time(59.999), "1:00.00");
        assert_eq!(cpu_time(3661.0), "1:01:01");
        assert_eq!(memory(Some(1024 * 1024)), "1.0 MiB");
        assert_eq!(exact_memory(Some(0)), "0 B (0 bytes)");
        assert_eq!(percent(None), "—");
        assert_eq!(elapsed_time(90061.0), "1d 01h 01m 01s");
    }
}
