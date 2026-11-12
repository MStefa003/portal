//! Portal for the desktop: send and receive files in a window, for people
//! who would rather not use the command line. It is installed separately
//! from the `portal` command, and transfers work between the two either way.
#![forbid(unsafe_code)]
// No console window behind the app on Windows (kept in debug builds, for logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod format;
mod logo;
mod style;
mod transfer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iced::widget::{
    button, checkbox, column, container, image, progress_bar, row, scrollable, space, text,
    text_input,
};
use iced::{
    Center, Color, Element, Fill, Font, Length, Size, Subscription, Task, Theme, event, window,
};
use portal_net::Route;
use portal_proto::Code;

use crate::format::{bytes, describe, duration};
use crate::transfer::{Counters, Net, Outcome, Reply, Update};

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Portal")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(560.0, 660.0),
            min_size: Some(Size::new(440.0, 540.0)),
            icon: logo::icon(),
            platform_specific: platform_specific(),
            ..window::Settings::default()
        })
        .centered()
        .run()
}

/// On Linux, the name the installed `portal-desktop.desktop` file matches,
/// so the dock and task switcher show Portal's icon for the window.
#[cfg(target_os = "linux")]
fn platform_specific() -> window::settings::PlatformSpecific {
    window::settings::PlatformSpecific {
        application_id: "portal-desktop".to_owned(),
        ..window::settings::PlatformSpecific::default()
    }
}

#[cfg(not(target_os = "linux"))]
fn platform_specific() -> window::settings::PlatformSpecific {
    window::settings::PlatformSpecific::default()
}

const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};

/// The two halves of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Send,
    Receive,
}

#[derive(Debug, Clone)]
enum Message {
    Tab(Tab),
    PickFiles,
    PickFolder,
    Picked(Vec<PathBuf>),
    Hovering(bool),
    Dropped(PathBuf),
    Remove(usize),
    StartSend,
    SendUpdate(Update),
    CodeChanged(String),
    StartReceive,
    ReceiveUpdate(Update),
    Answer(bool),
    Overwrite(bool),
    Cancel(Tab),
    Reset(Tab),
    ChangeDest,
    DestPicked(Option<PathBuf>),
    OpenFolder(PathBuf),
    Copy(String),
    Tick,
}

/// Where one side of the window is.
enum Stage {
    Idle,
    Busy(Job),
    Finished(Result<Outcome, String>),
}

/// A transfer under way. Dropping it stops the transfer.
struct Job {
    _handle: iced::task::Handle,
    counters: Arc<Counters>,
    /// What is being sent or received, once known.
    what: Option<String>,
    /// Bytes to read before the files are ready (sender only).
    read_total: u64,
    code: Option<String>,
    route: Option<Route>,
    flow: Option<Flow>,
    /// An offer waiting for the receiver's answer.
    offer: Option<Reply>,
    copied: bool,
}

/// Data flowing.
struct Flow {
    total: u64,
    skipped: u64,
    started: Instant,
}

/// A file or folder chosen for sending.
struct Chosen {
    path: PathBuf,
    /// The size of a file; `None` for a folder.
    size: Option<u64>,
}

struct App {
    theme: Theme,
    logo: Option<image::Handle>,
    tab: Tab,
    chosen: Vec<Chosen>,
    hovering: bool,
    send: Stage,
    code: String,
    code_error: Option<String>,
    overwrite: bool,
    receive: Stage,
    dest: PathBuf,
    notice: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            theme: style::theme(),
            logo: logo::image(),
            tab: Tab::Send,
            chosen: Vec::new(),
            hovering: false,
            send: Stage::Idle,
            code: String::new(),
            code_error: None,
            overwrite: false,
            receive: Stage::Idle,
            dest: config::download_dir(),
            notice: None,
        }
    }

    fn theme(&self) -> Theme {
        self.theme.clone()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tab(tab) => self.tab = tab,
            Message::PickFiles => {
                let pick = async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Choose files to send")
                        .pick_files()
                        .await
                        .unwrap_or_default()
                        .iter()
                        .map(|file| file.path().to_path_buf())
                        .collect()
                };
                return Task::perform(pick, Message::Picked);
            }
            Message::PickFolder => {
                let pick = async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Choose a folder to send")
                        .pick_folder()
                        .await
                        .map(|folder| vec![folder.path().to_path_buf()])
                        .unwrap_or_default()
                };
                return Task::perform(pick, Message::Picked);
            }
            Message::Picked(paths) => self.add(paths),
            Message::Hovering(hovering) => self.hovering = hovering,
            Message::Dropped(path) => {
                self.hovering = false;
                self.tab = Tab::Send;
                self.add(vec![path]);
            }
            Message::Remove(i) => {
                if i < self.chosen.len() {
                    self.chosen.remove(i);
                }
            }
            Message::StartSend => {
                if self.chosen.is_empty() || matches!(self.send, Stage::Busy(_)) {
                    return Task::none();
                }
                let counters = Arc::new(Counters::default());
                let paths = self.chosen.iter().map(|c| c.path.clone()).collect();
                let work = transfer::send(paths, Net::public(), counters.clone());
                let (task, handle) = Task::run(work, Message::SendUpdate).abortable();
                self.send = Stage::Busy(Job::new(handle, counters));
                return task;
            }
            Message::SendUpdate(update) => apply(&mut self.send, update),
            Message::CodeChanged(code) => {
                self.code = code;
                self.code_error = None;
            }
            Message::StartReceive => {
                if matches!(self.receive, Stage::Busy(_)) {
                    return Task::none();
                }
                // Checked before touching the network: a typo caught here
                // does not use up the sender's one attempt.
                let code = match Code::parse(&self.code) {
                    Ok(code) => code,
                    Err(e) => {
                        self.code_error = Some(capitalize(&e.to_string()));
                        return Task::none();
                    }
                };
                let counters = Arc::new(Counters::default());
                let dest = self.dest.clone();
                let work =
                    transfer::receive(code, dest, self.overwrite, Net::public(), counters.clone());
                let (task, handle) = Task::run(work, Message::ReceiveUpdate).abortable();
                self.receive = Stage::Busy(Job::new(handle, counters));
                return task;
            }
            Message::ReceiveUpdate(update) => apply(&mut self.receive, update),
            Message::Answer(accept) => {
                if let Stage::Busy(job) = &mut self.receive
                    && let Some(reply) = job.offer.take()
                {
                    reply.answer(accept);
                }
            }
            Message::Overwrite(overwrite) => self.overwrite = overwrite,
            Message::Cancel(tab) => *self.stage_mut(tab) = Stage::Idle,
            Message::Reset(tab) => {
                let stage = self.stage_mut(tab);
                let succeeded = matches!(stage, Stage::Finished(Ok(_)));
                *stage = Stage::Idle;
                if succeeded {
                    match tab {
                        Tab::Send => self.chosen.clear(),
                        Tab::Receive => self.code.clear(),
                    }
                }
            }
            Message::ChangeDest => {
                let pick = async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Where should received files go?")
                        .pick_folder()
                        .await
                        .map(|folder| folder.path().to_path_buf())
                };
                return Task::perform(pick, Message::DestPicked);
            }
            Message::DestPicked(Some(dir)) => {
                self.notice = config::set_download_dir(&dir).err();
                self.dest = dir;
            }
            Message::DestPicked(None) => {}
            Message::OpenFolder(dir) => open_folder(&dir),
            Message::Copy(contents) => {
                if let Stage::Busy(job) = &mut self.send {
                    job.copied = true;
                }
                return iced::clipboard::write(contents);
            }
            // Redraws, so the progress shown catches up with the counters.
            Message::Tick => {}
        }
        Task::none()
    }

    fn stage_mut(&mut self, tab: Tab) -> &mut Stage {
        match tab {
            Tab::Send => &mut self.send,
            Tab::Receive => &mut self.receive,
        }
    }

    /// Adds files to send, unless a transfer is under way.
    fn add(&mut self, paths: Vec<PathBuf>) {
        match self.send {
            Stage::Busy(_) => return,
            Stage::Finished(_) => {
                self.send = Stage::Idle;
                self.chosen.clear();
            }
            Stage::Idle => {}
        }
        for path in paths {
            if self.chosen.iter().any(|c| c.path == path) {
                continue;
            }
            let size = std::fs::metadata(&path)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            self.chosen.push(Chosen { path, size });
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let files = event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::FileHovered(_)) => Some(Message::Hovering(true)),
            iced::Event::Window(window::Event::FilesHoveredLeft) => Some(Message::Hovering(false)),
            iced::Event::Window(window::Event::FileDropped(path)) => Some(Message::Dropped(path)),
            _ => None,
        });
        let busy = [&self.send, &self.receive]
            .iter()
            .any(|stage| matches!(stage, Stage::Busy(_)));
        if busy {
            let tick = iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick);
            Subscription::batch([files, tick])
        } else {
            files
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tabs = container(
            row![
                tab_button("Send", Tab::Send, self.tab),
                tab_button("Receive", Tab::Receive, self.tab),
            ]
            .spacing(4),
        )
        .padding(4)
        .style(style::tabs);
        let mut brand = row![].spacing(10).align_y(Center);
        if let Some(logo) = &self.logo {
            brand = brand.push(image(logo.clone()).width(30).height(30));
        }
        brand = brand.push(text("Portal").size(22).font(BOLD));
        let header = row![brand, space::horizontal(), tabs].align_y(Center);

        let (title, subtitle, body) = match self.tab {
            Tab::Send => (
                "Send",
                "Files and folders go straight to the other computer, encrypted.",
                self.send_view(),
            ),
            Tab::Receive => (
                "Receive",
                "Type the code the sender sees to get their files.",
                self.receive_view(),
            ),
        };
        let heading = column![
            text(title).size(28).font(BOLD),
            text(subtitle).style(style::muted),
        ]
        .spacing(4);
        let content = column![header, heading, body]
            .spacing(28)
            .padding([28, 32])
            .max_width(600);
        container(scrollable(container(content).center_x(Fill)))
            .height(Fill)
            .into()
    }

    fn send_view(&self) -> Element<'_, Message> {
        match &self.send {
            Stage::Idle => self.choose_view(),
            Stage::Busy(job) => sending_view(job),
            Stage::Finished(Ok(outcome)) => {
                let mut detail = None;
                if let Outcome::Sent { skipped, .. } = outcome
                    && *skipped > 0
                {
                    detail = Some(format!(
                        "{skipped} {} could not be read and {} left out.",
                        if *skipped == 1 { "item" } else { "items" },
                        if *skipped == 1 { "was" } else { "were" },
                    ));
                }
                finished_view(
                    outcome_title(outcome),
                    detail,
                    row![primary("Send more", Message::Reset(Tab::Send))],
                )
            }
            Stage::Finished(Err(e)) => failed_view(e, Tab::Send),
        }
    }

    /// Picking what to send.
    fn choose_view(&self) -> Element<'_, Message> {
        let pickers = row![
            secondary("Choose files…", Message::PickFiles),
            secondary("Choose a folder…", Message::PickFolder),
        ]
        .spacing(10);
        let hint = if self.hovering {
            "Let go to add them"
        } else {
            "Drop files or folders here"
        };
        let zone = container(
            column![
                text(hint).size(19),
                text("or pick them yourself").style(style::muted),
                space().height(4),
                pickers,
            ]
            .spacing(8)
            .align_x(Center),
        )
        // Smaller once something is chosen, so the list and Send button fit.
        .padding(if self.chosen.is_empty() {
            [44, 24]
        } else {
            [24, 24]
        })
        .center_x(Fill)
        .style(style::drop_zone(self.hovering));

        let mut view = column![zone].spacing(16);
        if !self.chosen.is_empty() {
            let mut list = column![].spacing(2);
            for (i, chosen) in self.chosen.iter().enumerate() {
                let path = &chosen.path;
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let kind = chosen.size.map_or_else(|| "Folder".to_owned(), bytes);
                list = list.push(
                    row![
                        column![text(name), text(kind).size(13).style(style::muted)].spacing(2),
                        space::horizontal(),
                        button(text("Remove").size(13))
                            .style(style::quiet)
                            .on_press(Message::Remove(i)),
                    ]
                    .align_y(Center)
                    .padding([8, 4]),
                );
            }
            view = view.push(card(list.into(), [8, 16]));
        }
        let send = button(text("Send").size(17).font(BOLD).center().width(Fill))
            .width(Fill)
            .padding(14)
            .style(style::primary)
            .on_press_maybe((!self.chosen.is_empty()).then_some(Message::StartSend));
        view.push(send).into()
    }

    fn receive_view(&self) -> Element<'_, Message> {
        match &self.receive {
            Stage::Idle => self.code_view(),
            Stage::Busy(job) => receiving_view(job, &self.dest),
            Stage::Finished(Ok(outcome)) => {
                let again = secondary("Receive more", Message::Reset(Tab::Receive));
                match outcome {
                    Outcome::Files { dest, .. } => finished_view(
                        outcome_title(outcome),
                        Some(format!("Saved into {}", dest.display())),
                        row![
                            primary("Open folder", Message::OpenFolder(dest.clone())),
                            again
                        ]
                        .spacing(10),
                    ),
                    Outcome::Text(message) => {
                        let body = column![
                            card(text(message.as_str()).into(), [16, 18]),
                            row![primary("Copy", Message::Copy(message.clone())), again]
                                .spacing(10),
                        ]
                        .spacing(16);
                        column![
                            badge("✓", style::success_color()),
                            text(outcome_title(outcome)).size(20).font(BOLD),
                            body
                        ]
                        .spacing(16)
                        .into()
                    }
                    Outcome::Declined | Outcome::Sent { .. } => column![
                        text(outcome_title(outcome)).size(20).font(BOLD),
                        primary("Receive more", Message::Reset(Tab::Receive)),
                    ]
                    .spacing(16)
                    .into(),
                }
            }
            Stage::Finished(Err(e)) => failed_view(e, Tab::Receive),
        }
    }

    /// Typing the code, and where files will go.
    fn code_view(&self) -> Element<'_, Message> {
        let input = text_input("e.g. 7-cedar-river", &self.code)
            .on_input(Message::CodeChanged)
            .on_submit(Message::StartReceive)
            .padding(14)
            .size(20)
            .font(Font::MONOSPACE)
            .style(style::input);
        let mut code = column![text("Code").size(14).style(style::muted), input].spacing(8);
        if let Some(e) = &self.code_error {
            code = code.push(text(e.as_str()).size(14).style(style::danger));
        }
        let receive = button(text("Receive").size(17).font(BOLD).center().width(Fill))
            .width(Fill)
            .padding(14)
            .style(style::primary)
            .on_press_maybe((!self.code.trim().is_empty()).then_some(Message::StartReceive));

        let mut settings = column![
            text("Save received files into")
                .size(14)
                .style(style::muted),
            row![
                text(self.dest.display().to_string()).width(Fill),
                secondary("Change…", Message::ChangeDest),
            ]
            .spacing(12)
            .align_y(Center),
            checkbox(self.overwrite)
                .label("Replace files that already exist")
                .on_toggle(Message::Overwrite),
        ]
        .spacing(12);
        if let Some(notice) = &self.notice {
            settings = settings.push(text(notice.as_str()).size(14).style(style::danger));
        }
        column![code, receive, card(settings.into(), [18, 20])]
            .spacing(20)
            .into()
    }
}

impl Job {
    fn new(handle: iced::task::Handle, counters: Arc<Counters>) -> Self {
        Self {
            _handle: handle.abort_on_drop(),
            counters,
            what: None,
            read_total: 0,
            code: None,
            route: None,
            flow: None,
            offer: None,
            copied: false,
        }
    }
}

/// Records what a transfer reported.
fn apply(stage: &mut Stage, update: Update) {
    let Stage::Busy(job) = stage else {
        return;
    };
    match update {
        Update::Preparing { what, total } => {
            job.what = Some(what);
            job.read_total = total;
        }
        Update::Code(code) => job.code = Some(code),
        Update::Connected(route) => job.route = Some(route),
        Update::Offer { offer, reply } => {
            job.what = Some(format!(
                "\"{}\" ({})",
                offer.name,
                describe(offer.files, offer.bytes, false)
            ));
            job.offer = Some(reply);
        }
        Update::Started { total, skipped } => {
            job.flow = Some(Flow {
                total,
                skipped,
                started: Instant::now(),
            });
        }
        Update::Done(result) => *stage = Stage::Finished(result),
    }
}

fn sending_view(job: &Job) -> Element<'_, Message> {
    let what = job.what.as_deref().unwrap_or("your files");
    let mut view = column![].spacing(16);
    if let Some(flow) = &job.flow {
        let body = column![
            text(format!("Sending {what}")).font(BOLD),
            text(connected_label(job.route))
                .size(14)
                .style(style::muted),
            flow_view(job, flow, "already on the receiver or repeated"),
        ]
        .spacing(12);
        view = view.push(card(body.into(), [20, 22]));
    } else {
        if let Some(code) = &job.code {
            let copy = if job.copied { "Copied" } else { "Copy code" };
            let body = column![
                text("YOUR CODE").size(12).style(style::muted),
                text(code.as_str()).size(32).font(Font::MONOSPACE),
                primary(copy, Message::Copy(code.clone())),
                space().height(4),
                text("On the other computer, open Portal, choose Receive and type this code.")
                    .center(),
                text(format!("Or in a terminal there: portal receive {code}"))
                    .size(13)
                    .style(style::muted)
                    .center(),
            ]
            .spacing(10)
            .align_x(Center);
            view = view.push(
                container(body)
                    .padding([28, 24])
                    .center_x(Fill)
                    .style(style::card),
            );
        }
        let mut status = column![text(format!("Sending {what}"))].spacing(8);
        let read = job.counters.read.load(Ordering::Relaxed);
        if job.read_total > 0 && read < job.read_total {
            let fraction = read as f32 / job.read_total as f32;
            status = status.push(
                text(format!("Getting the files ready… {:.0}%", fraction * 100.0))
                    .size(14)
                    .style(style::muted),
            );
            status = status.push(progress_bar(0.0..=1.0, fraction).girth(6).style(style::bar));
        }
        let waiting = match job.route {
            Some(route) => connected_label(Some(route)),
            None if job.code.is_some() => "Waiting for the receiver…".to_owned(),
            None => "Looking at the files…".to_owned(),
        };
        status = status.push(text(waiting).size(14).style(style::muted));
        view = view.push(status);
    }
    view.push(secondary("Cancel", Message::Cancel(Tab::Send)))
        .into()
}

fn receiving_view<'a>(job: &'a Job, dest: &Path) -> Element<'a, Message> {
    let mut view = column![].spacing(16);
    if job.offer.is_some() {
        let what = job.what.as_deref().unwrap_or("files");
        let body = column![
            text("Incoming").size(12).style(style::muted),
            text(what.to_owned()).size(20).font(BOLD),
            text(format!("Save it into {}?", dest.display())).style(style::muted),
            space().height(4),
            row![
                primary("Accept", Message::Answer(true)),
                secondary("Decline", Message::Answer(false)),
            ]
            .spacing(10),
        ]
        .spacing(8);
        return view.push(card(body.into(), [22, 22])).into();
    }
    if let Some(flow) = &job.flow {
        let what = job.what.as_deref().unwrap_or("files");
        let body = column![
            text(format!("Receiving {what}")).font(BOLD),
            text(connected_label(job.route))
                .size(14)
                .style(style::muted),
            flow_view(job, flow, "already here or repeated"),
        ]
        .spacing(12);
        view = view.push(card(body.into(), [20, 22]));
    } else {
        let status = if job.route.is_some() {
            connected_label(job.route)
        } else {
            "Looking for the sender…".to_owned()
        };
        view = view.push(card(text(status).into(), [20, 22]));
    }
    view.push(secondary("Cancel", Message::Cancel(Tab::Receive)))
        .into()
}

/// The progress bar, speed and time left while data flows.
fn flow_view<'a>(job: &Job, flow: &Flow, why_skipped: &str) -> Element<'a, Message> {
    let moved = job.counters.moved.load(Ordering::Relaxed).min(flow.total);
    let fraction = if flow.total == 0 {
        1.0
    } else {
        moved as f32 / flow.total as f32
    };
    let elapsed = flow.started.elapsed().as_secs_f64();
    let speed = if elapsed > 0.5 {
        moved as f64 / elapsed
    } else {
        0.0
    };
    let mut details = row![text(format!("{:.0}%", fraction * 100.0)).font(BOLD)].spacing(16);
    details = details.push(space::horizontal());
    if speed > 0.0 {
        let left = Duration::from_secs_f64((flow.total - moved) as f64 / speed);
        details = details
            .push(text(format!("{}/s", bytes(speed as u64))).style(style::muted))
            .push(text(format!("{} left", duration(left))).style(style::muted));
    }
    let mut view = column![
        progress_bar(0.0..=1.0, fraction)
            .girth(10)
            .style(style::bar),
        details
    ]
    .spacing(10);
    if flow.skipped > 0 {
        view = view.push(
            text(format!(
                "{} {why_skipped}, not sent again",
                bytes(flow.skipped)
            ))
            .size(13)
            .style(style::muted),
        );
    }
    view.into()
}

/// A finished transfer: a check mark, what happened and what to do next.
fn finished_view<'a>(
    title: String,
    detail: Option<String>,
    actions: iced::widget::Row<'a, Message>,
) -> Element<'a, Message> {
    let mut view = column![
        badge("✓", style::success_color()),
        text(title).size(22).font(BOLD)
    ]
    .spacing(14);
    if let Some(detail) = detail {
        view = view.push(text(detail).style(style::muted));
    }
    view.push(space().height(4)).push(actions).into()
}

fn failed_view(error: &str, tab: Tab) -> Element<'_, Message> {
    column![
        badge("!", style::danger_color()),
        text("That did not work").size(22).font(BOLD),
        text(error).style(style::muted),
        space().height(4),
        primary("Try again", Message::Reset(tab)),
    ]
    .spacing(14)
    .into()
}

/// A round mark at the top of a finished screen.
fn badge<'a>(mark: &'a str, color: Color) -> Element<'a, Message> {
    container(text(mark).size(24).font(BOLD))
        .width(Length::Fixed(56.0))
        .height(Length::Fixed(56.0))
        .center(Length::Fixed(56.0))
        .style(style::badge(color))
        .into()
}

fn card<'a>(content: Element<'a, Message>, padding: [u16; 2]) -> Element<'a, Message> {
    container(content)
        .padding(padding)
        .width(Fill)
        .style(style::card)
        .into()
}

fn primary<'a>(label: &'a str, message: Message) -> Element<'a, Message> {
    button(text(label).font(BOLD))
        .padding([10, 22])
        .style(style::primary)
        .on_press(message)
        .into()
}

fn secondary<'a>(label: &'a str, message: Message) -> Element<'a, Message> {
    button(text(label))
        .padding([10, 18])
        .style(style::secondary)
        .on_press(message)
        .into()
}

fn tab_button(label: &str, tab: Tab, current: Tab) -> Element<'_, Message> {
    button(text(label).size(14).font(BOLD))
        .padding([7, 16])
        .style(style::tab(tab == current))
        .on_press(Message::Tab(tab))
        .into()
}

fn outcome_title(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Sent { what, .. } => format!("Sent {what}"),
        Outcome::Files { what, .. } => format!("Received {what}"),
        Outcome::Text(_) => "Received a text message".to_owned(),
        Outcome::Declined => "Declined. Nothing was saved.".to_owned(),
    }
}

fn connected_label(route: Option<Route>) -> String {
    match route {
        Some(Route::Direct(addr)) => format!("Connected directly ({})", addr.ip()),
        Some(Route::Relay) => "Connected through a relay".to_owned(),
        Some(Route::Unknown) | None => "Connected".to_owned(),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Opens `dir` in the file manager.
fn open_folder(dir: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(dir).spawn();
}
