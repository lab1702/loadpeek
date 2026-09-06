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

    fn width(self) -> f32 {
        match self {
            Self::Pid => 82.0,
            Self::User => 80.0,
            Self::Priority | Self::Nice => 42.0,
            Self::Virtual | Self::Resident | Self::Shared => 82.0,
            Self::State => 46.0,
            Self::Cpu => 66.0,
            Self::Memory => 64.0,
            Self::CpuTime => 86.0,
            Self::Command => 210.0,
        }
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
                    filters_changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.query)
                                .id_salt("process_search")
                                .hint_text("PID, user, name, or command")
                                .desired_width(270.0),
                        )
                        .labelled_by(search_label.id)
                        .changed();
                    let state_label = ui.label("State");
                    egui::ComboBox::from_id_salt("process_state")
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
                    if (!self.query.is_empty() || self.state != StateFilter::All)
                        && ui.button("Clear filters").clicked()
                    {
                        self.query.clear();
                        self.state = StateFilter::All;
                        filters_changed = true;
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
                    egui::CollapsingHeader::new(
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
        let minimum_width: f32 = Column::ALL.iter().map(|column| column.width()).sum();
        let table_width = ui.available_width().max(minimum_width);
        let command_width = Column::Command.width() + table_width - minimum_width;
        let viewport_height = ui.ctx().content_rect().height();
        let table_height = (viewport_height - 570.0).clamp(240.0, 430.0);
        let row_stride = ROW_HEIGHT + 2.0;
        let mut focus_target = None;
        let mut scroll_target = None;
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
                        for column in Column::ALL {
                            let width = if column == Column::Command {
                                command_width
                            } else {
                                column.width()
                            };
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
                    scroll.show_rows(ui, ROW_HEIGHT, processes.len(), |ui, range| {
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
                                    for column in Column::ALL {
                                        let width = if column == Column::Command {
                                            command_width
                                        } else {
                                            column.width()
                                        } - CELL_PADDING * 2.0;
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
                });
            });
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
                    if ui.button("Clear selection").clicked() {
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
    fn time_and_memory_values_preserve_units_and_boundary_rollover() {
        assert_eq!(cpu_time(59.999), "1:00.00");
        assert_eq!(cpu_time(3661.0), "1:01:01");
        assert_eq!(memory(Some(1024 * 1024)), "1.0 MiB");
        assert_eq!(exact_memory(Some(0)), "0 B (0 bytes)");
        assert_eq!(percent(None), "—");
        assert_eq!(elapsed_time(90061.0), "1d 01h 01m 01s");
    }
}
