use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, AtomExt, Color32, FontId, Layout, RichText, Stroke, Ui, Vec2,
};

use crate::{
    chart::{self, Series},
    history::History,
    metrics::{Collector, Snapshot},
    process_view::ProcessView,
    processes::{ProcessCollector, ProcessSnapshot},
    settings::Settings,
    theme::*,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Summary,
    Cpu,
    Memory,
    Disk,
    Network,
    Thermals,
    Processes,
}
impl Page {
    const ALL: [Self; 7] = [
        Self::Summary,
        Self::Cpu,
        Self::Memory,
        Self::Disk,
        Self::Network,
        Self::Thermals,
        Self::Processes,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Disk => "Disk",
            Self::Network => "Network",
            Self::Thermals => "Thermals",
            Self::Processes => "Processes",
        }
    }
    fn subtitle(self) -> &'static str {
        match self {
            Self::Summary => "Your system, at a glance.",
            Self::Cpu => "Processor activity, frequency, and every logical core.",
            Self::Memory => "Physical memory, reclaimable cache, and swap.",
            Self::Disk => "Read and write activity across your block devices.",
            Self::Network => "Incoming and outgoing traffic by interface.",
            Self::Thermals => "Temperature readings reported by your hardware.",
            Self::Processes => "A closer look at what is running on your system.",
        }
    }
    fn color(self) -> Color32 {
        match self {
            Self::Summary => MAUVE,
            Self::Cpu => BLUE,
            Self::Memory => MAUVE,
            Self::Disk => GREEN,
            Self::Network => TEAL,
            Self::Thermals => PEACH,
            Self::Processes => LAVENDER,
        }
    }
}

enum Command {
    Interval(f64),
    Pause(bool),
}
struct Sample {
    at: f64,
    snapshot: Snapshot,
    processes: ProcessSnapshot,
}

pub struct Loadpeek {
    page: Page,
    history: History,
    samples: mpsc::Receiver<Sample>,
    commands: mpsc::Sender<Command>,
    settings: Settings,
    paused: bool,
    settings_open: bool,
    notices_open: bool,
    config_notice: Option<String>,
    disk: String,
    network: String,
    core_filter: String,
    processes: ProcessSnapshot,
    process_view: ProcessView,
}

impl Loadpeek {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        let (settings, config_notice) = crate::settings::load();
        cc.egui_ctx.set_zoom_factor(settings.scale);
        let (sample_tx, samples) = mpsc::sync_channel(2);
        let (commands, command_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let mut interval = settings.refresh_secs;
        thread::Builder::new()
            .name("loadpeek-collector".into())
            .spawn(move || {
                let start = Instant::now();
                let mut collector = Collector::new();
                let mut process_collector = ProcessCollector::new();
                let mut paused = false;
                let mut next = Instant::now();
                loop {
                    if !paused && Instant::now() >= next {
                        let sample_start = Instant::now();
                        let mut snapshot = collector.sample();
                        let processes = process_collector.sample(snapshot.memory.total_bytes);
                        snapshot.warnings.extend(processes.warnings.iter().cloned());
                        let sample = Sample {
                            at: start.elapsed().as_secs_f64(),
                            snapshot,
                            processes,
                        };
                        match sample_tx.try_send(sample) {
                            Ok(()) => ctx.request_repaint(),
                            Err(mpsc::TrySendError::Disconnected(_)) => break,
                            Err(mpsc::TrySendError::Full(_)) => {}
                        }
                        next = sample_start + Duration::from_secs_f64(interval);
                    }
                    let timeout = if paused {
                        Duration::from_secs(60)
                    } else {
                        next.saturating_duration_since(Instant::now())
                    };
                    match command_rx.recv_timeout(timeout) {
                        Ok(Command::Interval(seconds)) => {
                            interval = seconds.clamp(0.5, 5.0);
                            next = Instant::now() + Duration::from_secs_f64(interval);
                        }
                        Ok(Command::Pause(value)) => {
                            paused = value;
                            if !paused {
                                collector = Collector::new();
                                process_collector = ProcessCollector::new();
                                next = Instant::now();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .expect("could not start metric collector");
        Self {
            page: Page::Summary,
            history: History::default(),
            samples,
            commands,
            settings,
            paused: false,
            settings_open: false,
            notices_open: false,
            config_notice,
            disk: String::new(),
            network: String::new(),
            core_filter: String::new(),
            processes: ProcessSnapshot::default(),
            process_view: ProcessView::default(),
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            while self.samples.try_recv().is_ok() {}
            self.history = History::default();
            self.processes = ProcessSnapshot::default();
        }
        let _ = self.commands.send(Command::Pause(self.paused));
    }
    fn persist(&mut self) {
        self.config_notice = self.settings.save().err();
    }
    fn series(
        &self,
        name: &str,
        color: Color32,
        dashed: bool,
        select: impl Fn(&Snapshot) -> Option<f64>,
    ) -> Series {
        Series {
            name: name.into(),
            color,
            dashed,
            points: self.history.series(select),
        }
    }

    fn navigation(&mut self, ui: &mut Ui, current: &Snapshot) {
        egui::Panel::left("navigation")
            .exact_size(208.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(CRUST).inner_margin(18))
            .show(ui, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, 8, MAUVE);
                    let points = [
                        (0.14, 0.57),
                        (0.3, 0.57),
                        (0.42, 0.28),
                        (0.56, 0.77),
                        (0.69, 0.44),
                        (0.87, 0.44),
                    ]
                    .map(|(x, y)| rect.min + rect.size() * Vec2::new(x, y));
                    p.add(egui::Shape::line(points.to_vec(), Stroke::new(2.2, CRUST)));
                    ui.label(RichText::new("loadpeek").size(25.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                small(ui, "LINUX SYSTEM MONITOR");
                ui.add_space(38.0);
                small(ui, "WORKSPACE");
                ui.add_space(12.0);
                // Center one shared two-column block, rather than centering each
                // differently sized number/name pair independently.
                let label_width = Page::ALL
                    .iter()
                    .map(|page| {
                        ui.painter()
                            .layout_no_wrap(page.name().into(), FontId::proportional(15.0), TEXT)
                            .size()
                            .x
                    })
                    .fold(0.0_f32, f32::max);
                for (index, page) in Page::ALL.into_iter().enumerate() {
                    let active = self.page == page;
                    let color = if active { MAUVE } else { SUBTEXT };
                    let number = RichText::new(format!("{:02}", index + 1))
                        .font(FontId::monospace(15.0))
                        .color(color)
                        .atom_size(Vec2::new(24.0, 20.0))
                        .atom_align(Align2::LEFT_CENTER);
                    let label = RichText::new(page.name())
                        .size(15.0)
                        .color(color)
                        .atom_size(Vec2::new(label_width, 20.0))
                        .atom_align(Align2::LEFT_CENTER);
                    let response = ui.add_sized(
                        [ui.available_width(), 44.0],
                        egui::Button::new((number, label))
                            .selected(active)
                            .fill(if active { SURFACE0 } else { CRUST })
                            .stroke(Stroke::NONE)
                            .corner_radius(8),
                    );
                    if active {
                        ui.painter().vline(
                            response.rect.left() + 1.0,
                            response.rect.top() + 12.0..=response.rect.bottom() - 12.0,
                            Stroke::new(3.0, MAUVE),
                        );
                    }
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            response.rect,
                            8,
                            Stroke::new(2.0, LAVENDER),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        self.page = page;
                    }
                    response.on_hover_text(format!("{} · Alt+{}", page.name(), index + 1));
                    ui.add_space(5.0);
                }
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.add_space(6.0);
                    small(ui, "Catppuccin Mocha");
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        for color in [MAUVE, BLUE, TEAL, GREEN, PEACH] {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, color);
                        }
                    });
                    ui.add_space(20.0);
                    small(ui, &format!("Linux {}", current.kernel));
                    ui.label(
                        RichText::new(if current.hostname.is_empty() {
                            "Local machine"
                        } else {
                            &current.hostname
                        })
                        .strong()
                        .color(TEXT),
                    );
                    ui.separator();
                    ui.add_space(12.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 36.0],
                            egui::Button::new("Settings & accessibility"),
                        )
                        .clicked()
                    {
                        self.settings_open = true;
                    }
                });
            });
    }

    fn header(&mut self, ui: &mut Ui, current: &Snapshot) {
        let compact = ui.available_width() < 560.0;
        ui.horizontal_wrapped(|ui| {
            if !compact {
                ui.label(RichText::new("SYSTEM / ").size(12.0).color(SUBTEXT));
                ui.label(
                    RichText::new(self.page.name().to_uppercase())
                        .size(12.0)
                        .color(self.page.color()),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(if self.paused { "Resume" } else { "Pause" })
                    .on_hover_text("Alt+P · Resume starts a fresh history")
                    .clicked()
                {
                    self.toggle_pause();
                }
                let before = self.settings.refresh_secs;
                let refresh_response = egui::ComboBox::from_id_salt("refresh_interval")
                    .width(78.0)
                    .selected_text(format!("{:.1} s", before))
                    .show_ui(ui, |ui| {
                        for half in 1..=10 {
                            let value = f64::from(half) / 2.0;
                            ui.selectable_value(
                                &mut self.settings.refresh_secs,
                                value,
                                format!("{value:.1} s"),
                            );
                        }
                    });
                let label = ui.label(RichText::new("Refresh").color(SUBTEXT));
                refresh_response.response.labelled_by(label.id);
                if before != self.settings.refresh_secs {
                    let _ = self
                        .commands
                        .send(Command::Interval(self.settings.refresh_secs));
                    self.persist();
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(if self.paused { "Paused" } else { "Live" })
                        .color(if self.paused { YELLOW } else { GREEN })
                        .size(13.0),
                );
            });
        });
        ui.add_space(18.0);
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(self.page.name())
                        .size(32.0)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(2.0);
                ui.label(RichText::new(self.page.subtitle()).color(SUBTEXT));
            });
            if ui.available_width() > 240.0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.vertical(|ui| {
                        small(ui, "UPTIME");
                        ui.label(
                            RichText::new(uptime(current.uptime_secs))
                                .size(18.0)
                                .color(TEXT),
                        );
                    });
                });
            }
        });
        ui.add_space(22.0);
    }

    fn summary(&mut self, ui: &mut Ui, s: &Snapshot) {
        let count = if ui.available_width() >= 1100.0 {
            3
        } else if ui.available_width() >= 740.0 {
            2
        } else {
            1
        };
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.spacing_mut().interact_size.y = 24.0;
        ui.spacing_mut().button_padding.y = 3.0;
        // Measure content before applying a common minimum, so every card
        // matches the tallest one even when labels wrap or the window resizes.
        let height_id = ui.id().with("summary_card_height");
        let card_height = ui
            .data(|data| data.get_temp::<f32>(height_id))
            .unwrap_or_default();
        let mut measured_height = 0.0_f32;
        for row in 0..(6 / count) {
            ui.columns(count, |columns| {
                for (col, ui) in columns.iter_mut().enumerate() {
                    let index = [0, 1, 4, 2, 3, 5][row * count + col];
                    card(ui, |ui| {
                        match index {
                            0 => {
                                self.card_title(ui, "CPU utilization", BLUE, Page::Cpu);
                                value(
                                    ui,
                                    percent(s.cpu_percent),
                                    &format!(
                                        "{} logical cores  ·  {}",
                                        s.cores.len(),
                                        frequency(cpu_frequency(s))
                                    ),
                                    BLUE,
                                );
                                chart::show(
                                    ui,
                                    "summary_cpu",
                                    &[self.series("CPU", BLUE, false, |s| s.cpu_percent)],
                                    98.0,
                                    Some(100.0),
                                    "%",
                                );
                            }
                            1 => {
                                self.card_title(ui, "Memory", MAUVE, Page::Memory);
                                value(
                                    ui,
                                    percent(memory_percent(s)),
                                    &if s.memory.total_bytes == 0 {
                                        "Memory counters unavailable".into()
                                    } else {
                                        format!(
                                            "{} of {} used",
                                            bytes(s.memory.used_bytes as f64),
                                            bytes(s.memory.total_bytes as f64)
                                        )
                                    },
                                    MAUVE,
                                );
                                chart::show(
                                    ui,
                                    "summary_memory",
                                    &[self.series("RAM", MAUVE, false, memory_percent)],
                                    98.0,
                                    Some(100.0),
                                    "%",
                                );
                            }
                            2 => {
                                self.card_title(ui, "Disk activity", GREEN, Page::Disk);
                                paired_values(
                                    ui,
                                    "Read",
                                    rate(disk_rate(s, "", false)),
                                    GREEN,
                                    "Write",
                                    rate(disk_rate(s, "", true)),
                                    PEACH,
                                );
                                chart::show(
                                    ui,
                                    "summary_disk",
                                    &[
                                        self.series("Read", GREEN, false, |s| {
                                            disk_rate(s, "", false)
                                        }),
                                        self.series("Write", PEACH, true, |s| {
                                            disk_rate(s, "", true)
                                        }),
                                    ],
                                    98.0,
                                    None,
                                    "B/s",
                                );
                            }
                            3 => {
                                self.card_title(ui, "Network traffic", TEAL, Page::Network);
                                paired_values(
                                    ui,
                                    "In",
                                    rate(network_rate(s, "", false)),
                                    TEAL,
                                    "Out",
                                    rate(network_rate(s, "", true)),
                                    LAVENDER,
                                );
                                chart::show(
                                    ui,
                                    "summary_network",
                                    &[
                                        self.series("In", TEAL, false, |s| {
                                            network_rate(s, "", false)
                                        }),
                                        self.series("Out", LAVENDER, true, |s| {
                                            network_rate(s, "", true)
                                        }),
                                    ],
                                    98.0,
                                    None,
                                    "B/s",
                                );
                            }
                            4 => {
                                self.card_title(ui, "CPU temperature", PEACH, Page::Thermals);
                                value(
                                    ui,
                                    temperature(cpu_temp(s)),
                                    if cpu_temp(s).is_some() {
                                        "Hottest reported CPU sensor"
                                    } else {
                                        "No CPU sensor exposed by this system"
                                    },
                                    PEACH,
                                );
                                chart::show(
                                    ui,
                                    "summary_temp",
                                    &[self.series("CPU temp", PEACH, false, cpu_temp)],
                                    98.0,
                                    None,
                                    "°C",
                                );
                            }
                            _ => {
                                self.card_title(ui, "System load", YELLOW, Page::Cpu);
                                ui.horizontal(|ui| {
                                    for (i, label) in
                                        ["1 min", "5 min", "15 min"].into_iter().enumerate()
                                    {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(number(s.load[i], 2))
                                                    .size(27.0)
                                                    .color(TEXT),
                                            );
                                            small(ui, label);
                                        });
                                        ui.add_space(24.0);
                                    }
                                });
                                ui.add_space(7.0);
                                chart::show(
                                    ui,
                                    "summary_load",
                                    &[self.series("1 min load", YELLOW, false, |s| {
                                        finite(s.load[0])
                                    })],
                                    98.0,
                                    None,
                                    "",
                                );
                            }
                        }
                        measured_height = measured_height.max(ui.min_rect().height());
                        ui.expand_to_include_rect(egui::Rect::from_min_size(
                            ui.min_rect().min,
                            Vec2::new(0.0, card_height),
                        ));
                    });
                }
            });
            ui.add_space(14.0);
        }
        if (card_height - measured_height).abs() > 0.5 {
            ui.data_mut(|data| data.insert_temp(height_id, measured_height));
            ui.ctx().request_discard("Equalize summary card heights");
        }
    }

    fn card_title(&mut self, ui: &mut Ui, title: &str, color: Color32, page: Page) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong().color(color).size(15.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("View {}", page.name()))
                                .size(12.0)
                                .color(SUBTEXT),
                        )
                        .frame(false),
                    )
                    .on_hover_text(format!("Open {} page", page.name()))
                    .clicked()
                {
                    self.page = page;
                }
            });
        });
        ui.add_space(7.0);
    }

    fn cpu(&self, ui: &mut Ui, s: &Snapshot) {
        ui.label(RichText::new(&s.cpu_model).color(SUBTEXT));
        ui.add_space(16.0);
        responsive_columns(ui, 2, |index, ui| {
            card(ui, |ui| {
                if index == 0 {
                    heading(ui, "Overall utilization", BLUE);
                    value(
                        ui,
                        percent(s.cpu_percent),
                        &format!("{} logical cores · total capacity = 100%", s.cores.len()),
                        BLUE,
                    );
                    chart::show(
                        ui,
                        "cpu_overall",
                        &[self.series("CPU", BLUE, false, |s| s.cpu_percent)],
                        180.0,
                        Some(100.0),
                        "%",
                    );
                } else {
                    heading(ui, "Clock speed", LAVENDER);
                    value(
                        ui,
                        frequency(cpu_frequency(s)),
                        "Mean of available logical core frequencies",
                        LAVENDER,
                    );
                    chart::show(
                        ui,
                        "cpu_clock",
                        &[self.series("Clock", LAVENDER, false, cpu_frequency)],
                        180.0,
                        None,
                        "MHz",
                    );
                }
            });
        });
        ui.add_space(18.0);
        card(ui, |ui| {
            heading(ui, "Load averages", YELLOW);
            ui.horizontal_wrapped(|ui| {
                for (i, label) in ["1 minute", "5 minutes", "15 minutes"]
                    .into_iter()
                    .enumerate()
                {
                    ui.label(
                        RichText::new(format!("{}  {}", number(s.load[i], 2), label))
                            .size(19.0)
                            .color(TEXT),
                    );
                    ui.add_space(22.0);
                }
            });
            small(
                ui,
                "Runnable or uninterruptible tasks; load is not a CPU percentage. Compare with the number of logical cores.",
            );
        });
    }

    fn core_charts(&mut self, ui: &mut Ui, s: &Snapshot) {
        ui.add_space(24.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Logical cores").size(22.0).strong());
            ui.label(RichText::new(format!("{} online", s.cores.len())).color(SUBTEXT));
            ui.add_space(18.0);
            let label = ui.label("Filter core:");
            ui.add(
                egui::TextEdit::singleline(&mut self.core_filter)
                    .hint_text("e.g. 12")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        });
        ui.add_space(12.0);
        let cores: Vec<_> = s
            .cores
            .iter()
            .filter(|c| {
                self.core_filter.trim().is_empty()
                    || c.id.to_string().contains(self.core_filter.trim())
            })
            .collect();
        let columns = ((ui.available_width() / 275.0) as usize).clamp(1, 4);
        if cores.is_empty() {
            ui.label("No cores match this filter.");
        }
        for chunk in cores.chunks(columns) {
            ui.columns(columns, |uis| {
                for (core, ui) in chunk.iter().zip(uis) {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Core {:02}", core.id))
                                    .color(BLUE)
                                    .strong(),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(percent(core.percent)).size(19.0).color(TEXT),
                                );
                            });
                        });
                        small(ui, &frequency(core.frequency_mhz));
                        chart::show(
                            ui,
                            ("core", core.id),
                            &[self.series("Use", BLUE, false, |s| {
                                s.cores
                                    .iter()
                                    .find(|c| c.id == core.id)
                                    .and_then(|c| c.percent)
                            })],
                            78.0,
                            Some(100.0),
                            "%",
                        );
                    });
                }
            });
            ui.add_space(12.0);
        }
    }

    fn memory(&self, ui: &mut Ui, s: &Snapshot) {
        if s.memory.total_bytes == 0 {
            card(ui, |ui| {
                heading(ui, "Memory unavailable", MAUVE);
                ui.label("Readable memory counters are not available. See the availability notices for details.");
            });
            return;
        }
        responsive_columns(ui, 2, |index, ui| {
            card(ui, |ui| {
                if index == 0 {
                    heading(ui, "Physical memory", MAUVE);
                    value(
                        ui,
                        percent(memory_percent(s)),
                        &format!(
                            "{} / {}",
                            bytes(s.memory.used_bytes as f64),
                            bytes(s.memory.total_bytes as f64)
                        ),
                        MAUVE,
                    );
                    chart::show(
                        ui,
                        "ram_detail",
                        &[self.series("Used", MAUVE, false, memory_percent)],
                        230.0,
                        Some(100.0),
                        "%",
                    );
                } else {
                    heading(ui, "Swap", LAVENDER);
                    value(
                        ui,
                        if s.memory.swap_total_bytes == 0 {
                            "Not configured".into()
                        } else {
                            percent(swap_percent(s))
                        },
                        &format!(
                            "{} / {}",
                            bytes(s.memory.swap_used_bytes as f64),
                            bytes(s.memory.swap_total_bytes as f64)
                        ),
                        LAVENDER,
                    );
                    chart::show(
                        ui,
                        "swap_detail",
                        &[self.series("Swap", LAVENDER, false, swap_percent)],
                        230.0,
                        Some(100.0),
                        "%",
                    );
                }
            })
        });
        ui.add_space(18.0);
        card(ui, |ui| {
            heading(ui, "Memory breakdown", MAUVE);
            metric_row(ui, "Total installed", bytes(s.memory.total_bytes as f64));
            metric_row(ui, "In use", bytes(s.memory.used_bytes as f64));
            metric_row(
                ui,
                "Available to applications",
                bytes(s.memory.available_bytes as f64),
            );
            metric_row(
                ui,
                "Cache & reclaimable slab",
                bytes(s.memory.cached_bytes as f64),
            );
            ui.add_space(12.0);
            small(
                ui,
                "In use = total − MemAvailable. Available includes reclaimable memory; cache is not an additional allocation.",
            );
        });
    }

    fn disk(&mut self, ui: &mut Ui, s: &Snapshot) {
        if !self.disk.is_empty() && !s.disks.iter().any(|d| d.name == self.disk) {
            self.disk.clear();
        }
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Device");
            egui::ComboBox::from_id_salt("disk_selector")
                .selected_text(if self.disk.is_empty() {
                    "All devices"
                } else {
                    &self.disk
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.disk, String::new(), "All devices");
                    for d in &s.disks {
                        ui.selectable_value(&mut self.disk, d.name.clone(), &d.name);
                    }
                })
                .response
                .labelled_by(label.id);
            small(ui, "Whole leaf block devices · partitions excluded");
        });
        ui.add_space(16.0);
        card(ui, |ui| {
            heading(ui, "Disk throughput", GREEN);
            paired_values(
                ui,
                "Read",
                rate(disk_rate(s, &self.disk, false)),
                GREEN,
                "Write",
                rate(disk_rate(s, &self.disk, true)),
                PEACH,
            );
            chart::show(
                ui,
                "disk_detail",
                &[
                    self.series("Read", GREEN, false, |s| disk_rate(s, &self.disk, false)),
                    self.series("Write", PEACH, true, |s| disk_rate(s, &self.disk, true)),
                ],
                250.0,
                None,
                "B/s",
            );
        });
        ui.add_space(22.0);
        heading(ui, "Devices", GREEN);
        if s.disks.is_empty() {
            ui.label("No readable block device counters are available.");
        }
        for disk in &s.disks {
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&disk.name).size(19.0).strong());
                    ui.add_space(24.0);
                    ui.label(format!("Read  {}", rate(disk.read_bytes_per_sec)));
                    ui.add_space(24.0);
                    ui.label(format!("Write  {}", rate(disk.write_bytes_per_sec)));
                });
                small(
                    ui,
                    &format!(
                        "Since device start: {} read · {} written",
                        bytes(disk.total_read_bytes as f64),
                        bytes(disk.total_write_bytes as f64)
                    ),
                );
            });
            ui.add_space(10.0);
        }
    }

    fn network(&mut self, ui: &mut Ui, s: &Snapshot) {
        if !self.network.is_empty() && !s.networks.iter().any(|d| d.name == self.network) {
            self.network.clear();
        }
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Interface");
            egui::ComboBox::from_id_salt("network_selector")
                .selected_text(if self.network.is_empty() {
                    "All interfaces"
                } else {
                    &self.network
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.network, String::new(), "All interfaces");
                    for n in &s.networks {
                        ui.selectable_value(&mut self.network, n.name.clone(), &n.name);
                    }
                })
                .response
                .labelled_by(label.id);
            small(ui, "Loopback excluded · rates in bytes per second");
        });
        ui.add_space(16.0);
        card(ui, |ui| {
            heading(ui, "Network throughput", TEAL);
            paired_values(
                ui,
                "Incoming",
                rate(network_rate(s, &self.network, false)),
                TEAL,
                "Outgoing",
                rate(network_rate(s, &self.network, true)),
                LAVENDER,
            );
            chart::show(
                ui,
                "network_detail",
                &[
                    self.series("In", TEAL, false, |s| network_rate(s, &self.network, false)),
                    self.series("Out", LAVENDER, true, |s| {
                        network_rate(s, &self.network, true)
                    }),
                ],
                250.0,
                None,
                "B/s",
            );
            small(
                ui,
                "All interfaces sums interface counters. Bridges, VPNs, and virtual adapters can count the same traffic more than once.",
            );
        });
        ui.add_space(22.0);
        heading(ui, "Interfaces", TEAL);
        if s.networks.is_empty() {
            ui.label("No non-loopback network interfaces are available.");
        }
        for n in &s.networks {
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&n.name).size(19.0).strong());
                    ui.label(
                        RichText::new(if n.is_up { "Up" } else { "Down" }).color(if n.is_up {
                            GREEN
                        } else {
                            SUBTEXT
                        }),
                    );
                    ui.add_space(18.0);
                    ui.label(format!("In  {}", rate(n.received_bytes_per_sec)));
                    ui.add_space(18.0);
                    ui.label(format!("Out  {}", rate(n.transmitted_bytes_per_sec)));
                });
                small(
                    ui,
                    &format!(
                        "Since interface start: {} received · {} sent",
                        bytes(n.total_received_bytes as f64),
                        bytes(n.total_transmitted_bytes as f64)
                    ),
                );
            });
            ui.add_space(10.0);
        }
    }

    fn thermals(&self, ui: &mut Ui, s: &Snapshot) {
        card(ui, |ui| {
            heading(ui, "CPU temperature", PEACH);
            value(
                ui,
                temperature(cpu_temp(s)),
                "Hottest CPU sensor at each sample",
                PEACH,
            );
            chart::show(
                ui,
                "thermal_cpu",
                &[self.series("CPU temp", PEACH, false, cpu_temp)],
                200.0,
                None,
                "°C",
            );
            if cpu_temp(s).is_none() {
                small(
                    ui,
                    "CPU temperature is unavailable. A sensor driver or host access to /sys may be needed.",
                );
            }
        });
        ui.add_space(24.0);
        ui.label(
            RichText::new(format!("Hardware sensors  ·  {}", s.sensors.len()))
                .size(22.0)
                .strong(),
        );
        ui.add_space(12.0);
        if s.sensors.is_empty() {
            card(ui, |ui| {
                heading(ui, "No temperature sensors detected", PEACH);
                ui.label("This machine does not expose readable hwmon or thermal-zone temperatures. Other monitoring pages remain available.");
            });
        }
        let columns = if ui.available_width() >= 770.0 { 2 } else { 1 };
        for sensors in s.sensors.chunks(columns) {
            ui.columns(columns, |uis| {
                for (sensor, ui) in sensors.iter().zip(uis) {
                    card(ui, |ui| {
                        heading(ui, &sensor.label, PEACH);
                        let critical = sensor.critical_celsius.is_some_and(|v| sensor.celsius >= v);
                        let subtitle = match sensor.critical_celsius {
                            Some(v) if critical => {
                                format!("At or above critical threshold ({v:.0} °C)")
                            }
                            Some(v) => format!("Below reported critical threshold ({v:.0} °C)"),
                            None => "Critical threshold not reported".into(),
                        };
                        value(
                            ui,
                            temperature(Some(sensor.celsius)),
                            &subtitle,
                            if critical { RED } else { PEACH },
                        );
                        chart::show(
                            ui,
                            ("sensor", &sensor.id),
                            &[self.series("Temp", PEACH, false, |s| {
                                s.sensors
                                    .iter()
                                    .find(|v| v.id == sensor.id)
                                    .map(|v| v.celsius)
                            })],
                            110.0,
                            None,
                            "°C",
                        );
                    });
                }
            });
            ui.add_space(14.0);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Settings & accessibility").open(&mut open).resizable(true).default_width(460.0).max_width((ctx.content_rect().width() - 32.0).max(220.0)).max_height((ctx.content_rect().height() - 60.0).max(120.0)).vscroll(true).show(ctx, |ui| {
            heading(ui, "Display", MAUVE);
            let label = ui.label("Text and interface size");
            let before = self.settings.scale;
            egui::ComboBox::from_id_salt("interface_scale").selected_text(format!("{:.0}%", self.settings.scale * 100.0)).show_ui(ui, |ui| {
                for scale in [1.0, 1.25, 1.5, 1.75, 2.0] { ui.selectable_value(&mut self.settings.scale, scale, format!("{:.0}%", scale * 100.0)); }
            }).response.labelled_by(label.id);
            if self.settings.scale != before { ctx.set_zoom_factor(self.settings.scale); self.persist(); }
            ui.add_space(16.0);
            heading(ui, "Keyboard", LAVENDER);
            for (key, action) in [("Tab / Shift+Tab", "Move between controls"), ("Enter / Space", "Activate the focused control"), ("Alt+1 … Alt+7", "Switch pages"), ("Alt+P", "Pause or resume samples"), ("Alt+S", "Open settings"), ("Arrow keys", "Navigate open menus"), ("Up / Down on a process row", "Select previous / next process"), ("Page Up / Down on a process row", "Move one page through processes"), ("Home / End on a process row", "Select first / last process")] { metric_row(ui, key, action.into()); }
            ui.add_space(16.0);
            ui.label("Charts use labels and line patterns as well as color. Current values and history statistics are also available as text. Screen reader support uses AccessKit.");
            ui.add_space(10.0);
            small(ui, "Pause freezes the display. Resume starts a new 60-second history. Sampling continues across pages. Settings are saved locally.");
        });
        self.settings_open = open;
    }
}

impl eframe::App for Loadpeek {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(sample) = self.samples.try_recv() {
            if !self.paused {
                self.history.push(sample.at, sample.snapshot);
                // The process list is a current view, not a 60-second archive
                // of thousands of command lines.
                self.processes = sample.processes;
            }
        }
        ctx.input_mut(|input| {
            for (key, page) in [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
            ]
            .into_iter()
            .zip(Page::ALL)
            {
                if input.consume_key(egui::Modifiers::ALT, key) {
                    self.page = page;
                }
            }
            if input.consume_key(egui::Modifiers::ALT, egui::Key::P) {
                self.toggle_pause();
            }
            if input.consume_key(egui::Modifiers::ALT, egui::Key::S) {
                self.settings_open = !self.settings_open;
            }
        });
        let s = self.history.latest().cloned().unwrap_or_else(|| Snapshot {
            uptime_secs: f64::NAN,
            load: [f64::NAN; 3],
            ..Snapshot::default()
        });
        // Keep the seven navigation rows clear of the sidebar footer.
        let wide = ui.available_width() >= 900.0 && ui.available_height() >= 840.0;
        if wide {
            self.navigation(ui, &s);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BASE)
                    .inner_margin(if wide { 28 } else { 16 }),
            )
            .show(ui, |ui| {
                if !wide {
                    let compact_navigation = ui.available_width() < 550.0;
                    ui.horizontal_wrapped(|ui| {
                        if compact_navigation {
                            let label = ui.label("Page");
                            egui::ComboBox::from_id_salt("compact_page")
                                .selected_text(self.page.name())
                                .width(100.0)
                                .show_ui(ui, |ui| {
                                    for page in Page::ALL {
                                        ui.selectable_value(&mut self.page, page, page.name());
                                    }
                                })
                                .response
                                .labelled_by(label.id);
                        } else {
                            for page in Page::ALL {
                                if ui
                                    .selectable_label(self.page == page, page.name())
                                    .clicked()
                                {
                                    self.page = page;
                                }
                            }
                        }
                        if ui.button("Settings").clicked() {
                            self.settings_open = true;
                        }
                    });
                    ui.add_space(12.0);
                }
                egui::ScrollArea::vertical()
                    .id_salt(("page_scroll", self.page.name()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.header(ui, &s);
                        let notice_count =
                            s.warnings.len() + usize::from(self.config_notice.is_some());
                        ui.horizontal_wrapped(|ui| {
                            small(
                                ui,
                                &if self.page == Page::Processes {
                                    if self.paused {
                                        "PAUSED PROCESS SNAPSHOT".into()
                                    } else {
                                        format!(
                                            "CURRENT PROCESSES  ·  refresh every {:.1}s",
                                            self.settings.refresh_secs
                                        )
                                    }
                                } else {
                                    format!(
                                        "LAST 60 SECONDS  ·  {:.0}s collected  ·  {} samples",
                                        self.history.span_seconds().min(60.0),
                                        self.history.len()
                                    )
                                },
                            );
                            if notice_count > 0
                                && ui
                                    .button(
                                        RichText::new(format!(
                                            "{notice_count} availability notices"
                                        ))
                                        .size(12.0)
                                        .color(YELLOW),
                                    )
                                    .clicked()
                            {
                                self.notices_open = true;
                            }
                        });
                        ui.add_space(14.0);

                        match self.page {
                            Page::Summary => self.summary(ui, &s),
                            Page::Cpu => {
                                self.cpu(ui, &s);
                                self.core_charts(ui, &s);
                            }
                            Page::Memory => self.memory(ui, &s),
                            Page::Disk => self.disk(ui, &s),
                            Page::Network => self.network(ui, &s),
                            Page::Thermals => self.thermals(ui, &s),
                            Page::Processes => self.process_view.show(ui, &self.processes),
                        }
                        ui.add_space(12.0);
                        small(ui, "LOCAL METRICS  /  No account. No telemetry.");
                    });
            });
        self.settings_window(&ctx);
        egui::Window::new("Metric availability")
            .open(&mut self.notices_open)
            .default_width(480.0)
            .max_width((ctx.content_rect().width() - 32.0).max(220.0))
            .max_height((ctx.content_rect().height() - 60.0).max(120.0))
            .vscroll(true)
            .show(&ctx, |ui| {
                for warning in &s.warnings {
                    ui.label(format!("• {warning}"));
                    ui.add_space(8.0);
                }
                if let Some(warning) = &self.config_notice {
                    ui.label(warning);
                }
            });
    }
}

fn card(ui: &mut Ui, contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(MANTLE)
        .stroke(Stroke::new(1.0, SURFACE0))
        .corner_radius(12)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
}
fn small(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(SUBTEXT));
}
fn heading(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).size(16.0).strong().color(color));
    ui.add_space(7.0);
}
fn value(ui: &mut Ui, primary: String, secondary: &str, color: Color32) {
    ui.label(
        RichText::new(primary)
            .font(FontId::proportional(30.0))
            .color(color),
    );
    small(ui, secondary);
    ui.add_space(9.0);
}
fn paired_values(
    ui: &mut Ui,
    left: &str,
    lv: String,
    lc: Color32,
    right: &str,
    rv: String,
    rc: Color32,
) {
    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(lv).size(25.0).color(lc));
            small(ui, left);
        });
        ui.add_space(25.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(rv).size(25.0).color(rc));
            small(ui, right);
        });
    });
    ui.add_space(9.0);
}
fn metric_row(ui: &mut Ui, label: &str, value: String) {
    if ui.available_width() < 420.0 {
        ui.label(RichText::new(label).color(SUBTEXT));
        ui.label(RichText::new(value).color(TEXT));
        ui.add_space(7.0);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(label).color(SUBTEXT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).color(TEXT));
        });
    });
    ui.add_space(7.0);
}
fn responsive_columns(ui: &mut Ui, count: usize, mut content: impl FnMut(usize, &mut Ui)) {
    if ui.available_width() > 770.0 {
        ui.columns(count, |uis| {
            for (i, ui) in uis.iter_mut().enumerate() {
                content(i, ui);
            }
        });
    } else {
        for i in 0..count {
            content(i, ui);
            ui.add_space(14.0);
        }
    }
}
fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}
fn memory_percent(s: &Snapshot) -> Option<f64> {
    (s.memory.total_bytes > 0)
        .then(|| s.memory.used_bytes as f64 / s.memory.total_bytes as f64 * 100.0)
}
fn swap_percent(s: &Snapshot) -> Option<f64> {
    (s.memory.swap_total_bytes > 0)
        .then(|| s.memory.swap_used_bytes as f64 / s.memory.swap_total_bytes as f64 * 100.0)
}
fn cpu_frequency(s: &Snapshot) -> Option<f64> {
    let frequencies: Vec<_> = s
        .cores
        .iter()
        .filter_map(|c| c.frequency_mhz)
        .filter(|f| f.is_finite())
        .collect();
    (!frequencies.is_empty()).then(|| frequencies.iter().sum::<f64>() / frequencies.len() as f64)
}
fn cpu_temp(s: &Snapshot) -> Option<f64> {
    s.sensors
        .iter()
        .filter(|s| s.is_cpu)
        .map(|s| s.celsius)
        .filter(|v| v.is_finite())
        .reduce(f64::max)
}
fn disk_rate(s: &Snapshot, name: &str, write: bool) -> Option<f64> {
    sum_rates(
        s.disks
            .iter()
            .filter(|d| name.is_empty() || d.name == name)
            .map(|d| {
                if write {
                    d.write_bytes_per_sec
                } else {
                    d.read_bytes_per_sec
                }
            }),
    )
}
fn network_rate(s: &Snapshot, name: &str, transmit: bool) -> Option<f64> {
    sum_rates(
        s.networks
            .iter()
            .filter(|n| name.is_empty() || n.name == name)
            .map(|n| {
                if transmit {
                    n.transmitted_bytes_per_sec
                } else {
                    n.received_bytes_per_sec
                }
            }),
    )
}
fn sum_rates(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut count = 0;
    let mut sum = 0.0;
    for value in values {
        sum += value.filter(|v| v.is_finite())?;
        count += 1;
    }
    (count > 0).then_some(sum)
}
fn number(value: f64, decimals: usize) -> String {
    if value.is_finite() {
        format!("{value:.decimals$}")
    } else {
        "Unavailable".into()
    }
}
fn percent(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "Unavailable".into())
}
fn frequency(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.2} GHz", v / 1000.0))
        .unwrap_or_else(|| "Clock unavailable".into())
}
fn temperature(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1} °C"))
        .unwrap_or_else(|| "Unavailable".into())
}
fn bytes(value: f64) -> String {
    if !value.is_finite() {
        return "Unavailable".into();
    }
    let mut value = value.max(0.0);
    let mut i = 0;
    while value >= 1024.0 && i < 5 {
        value /= 1024.0;
        i += 1;
    }
    let unit = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"][i];
    if i == 0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}
fn rate(value: Option<f64>) -> String {
    value
        .map(|v| format!("{}/s", bytes(v)))
        .unwrap_or_else(|| "Unavailable".into())
}
fn uptime(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "Unavailable".into();
    }
    let seconds = seconds.max(0.0) as u64;
    let days = seconds / 86400;
    let hours = seconds % 86400 / 3600;
    let minutes = seconds % 3600 / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregation_does_not_silently_report_partial_totals() {
        assert_eq!(sum_rates([Some(2.0), Some(3.0)].into_iter()), Some(5.0));
        assert_eq!(sum_rates([Some(2.0), None].into_iter()), None);
        assert_eq!(sum_rates(std::iter::empty()), None);
        assert_eq!(sum_rates([Some(f64::NAN)].into_iter()), None);
    }
    #[test]
    fn absent_memory_is_not_zero_percent() {
        assert_eq!(memory_percent(&Snapshot::default()), None);
    }
    #[test]
    fn binary_units_and_uptime() {
        assert_eq!(bytes(1024.0 * 1024.0), "1.0 MiB");
        assert_eq!(uptime(90061.0), "1d 1h 1m");
        assert_eq!(uptime(f64::NAN), "Unavailable");
    }
}
