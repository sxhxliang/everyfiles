use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::deleter::SafeDeleter;
use crate::domain::format_size;
use crate::guard::SafetyGuard;
use crate::scanners::dir_size_and_count;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub percent: f64,
    pub file_count: usize,
}

pub struct SpaceLens;

impl SpaceLens {
    /// Explore direct children of a directory, computing sizes and percentages in parallel.
    pub fn explore(dir: &Path) -> (u64, Vec<SpaceNode>) {
        if !dir.exists() || !dir.is_dir() {
            return (0, Vec::new());
        }

        let entries = match fs::read_dir(dir) {
            Ok(e) => e.flatten().collect::<Vec<_>>(),
            Err(_) => return (0, Vec::new()),
        };

        let mut nodes: Vec<SpaceNode> = entries
            .into_par_iter()
            .map(|entry| {
                let path = entry.path();
                let is_dir = path.is_dir();
                let name = entry.file_name().to_string_lossy().to_string();
                let (size_bytes, file_count) = dir_size_and_count(&path);
                SpaceNode {
                    name,
                    path,
                    is_dir,
                    size_bytes,
                    percent: 0.0,
                    file_count,
                }
            })
            .collect();

        // Sort by size descending
        nodes.sort_by_key(|n| std::cmp::Reverse(n.size_bytes));

        let total_size: u64 = nodes.iter().map(|n| n.size_bytes).sum();
        if total_size > 0 {
            for node in &mut nodes {
                node.percent = (node.size_bytes as f64 / total_size as f64) * 100.0;
            }
        }

        (total_size, nodes)
    }

    /// Render a DaisyDisk-style visual bar.
    pub fn render_bar(percent: f64, width: usize) -> String {
        let filled = ((percent / 100.0) * width as f64).round() as usize;
        let empty = width.saturating_sub(filled);
        format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
    }

    /// Launch interactive DaisyDisk-style TUI explorer.
    pub fn launch_interactive(initial_path: PathBuf) -> io::Result<()> {
        let mut app = SpaceLensApp::new(initial_path);
        app.run()
    }
}

pub struct SpaceLensApp {
    current_dir: PathBuf,
    total_size: u64,
    nodes: Vec<SpaceNode>,
    selected_idx: usize,
    history_stack: Vec<PathBuf>,
    status_message: String,
    confirm_delete: Option<SpaceNode>,
}

impl SpaceLensApp {
    pub fn new(start_dir: PathBuf) -> Self {
        let (total_size, nodes) = SpaceLens::explore(&start_dir);
        Self {
            current_dir: start_dir,
            total_size,
            nodes,
            selected_idx: 0,
            history_stack: Vec::new(),
            status_message: "就绪。按 [Enter] 下钻，[Backspace] 返回上一层，[D] 移至废纸篓".into(),
            confirm_delete: None,
        }
    }

    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let res = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        res
    }

    fn event_loop<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        if self.confirm_delete.is_some() {
                            match key.code {
                                KeyCode::Char('y') | KeyCode::Enter => {
                                    if let Some(target) = self.confirm_delete.take() {
                                        match SafeDeleter::clean_path(&target.path, false, false) {
                                            Ok(reclaimed) => {
                                                self.status_message = format!(
                                                    "已安全移至废纸篓: {} ({})",
                                                    target.name,
                                                    format_size(reclaimed)
                                                );
                                                self.refresh();
                                            }
                                            Err(e) => {
                                                self.status_message = format!("删除失败: {e}");
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    self.confirm_delete = None;
                                }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    if self.history_stack.is_empty() {
                                        return Ok(());
                                    } else {
                                        self.go_back();
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if self.selected_idx > 0 {
                                        self.selected_idx -= 1;
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if !self.nodes.is_empty()
                                        && self.selected_idx + 1 < self.nodes.len()
                                    {
                                        self.selected_idx += 1;
                                    }
                                }
                                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                                    self.drill_down();
                                }
                                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                                    self.go_back();
                                }
                                KeyCode::Char('d') | KeyCode::Delete => {
                                    if self.selected_idx < self.nodes.len() {
                                        let node = self.nodes[self.selected_idx].clone();
                                        if let Err(reason) = SafetyGuard::validate(&node.path) {
                                            self.status_message = format!("禁止删除: {reason}");
                                        } else {
                                            self.confirm_delete = Some(node);
                                        }
                                    }
                                }
                                KeyCode::Char('o') => {
                                    if self.selected_idx < self.nodes.len() {
                                        let p = &self.nodes[self.selected_idx].path;
                                        let _ = Command::new("open").arg("-R").arg(p).output();
                                        self.status_message =
                                            format!("已在 Finder 中定位: {}", p.display());
                                    }
                                }
                                KeyCode::Char('r') => {
                                    self.refresh();
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    fn drill_down(&mut self) {
        if self.selected_idx < self.nodes.len() {
            let node = &self.nodes[self.selected_idx];
            if node.is_dir {
                let next_path = node.path.clone();
                self.history_stack.push(self.current_dir.clone());
                self.current_dir = next_path;
                let (tot, nodes) = SpaceLens::explore(&self.current_dir);
                self.total_size = tot;
                self.nodes = nodes;
                self.selected_idx = 0;
            }
        }
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history_stack.pop() {
            self.current_dir = prev;
            let (tot, nodes) = SpaceLens::explore(&self.current_dir);
            self.total_size = tot;
            self.nodes = nodes;
            self.selected_idx = 0;
        }
    }

    fn refresh(&mut self) {
        let (tot, nodes) = SpaceLens::explore(&self.current_dir);
        self.total_size = tot;
        self.nodes = nodes;
        if self.selected_idx >= self.nodes.len() {
            self.selected_idx = self.nodes.len().saturating_sub(1);
        }
    }

    fn ui(&mut self, f: &mut Frame) {
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(size);

        // Header with breadcrumb
        let header_line = Line::from(vec![
            Span::styled(
                " 磁盘空间透视器 (Space Lens) ",
                Style::default()
                    .bg(Color::Rgb(249, 226, 175))
                    .fg(Color::Rgb(30, 30, 46))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" 路径: "),
            Span::styled(
                self.current_dir.display().to_string(),
                Style::default()
                    .fg(Color::Rgb(137, 180, 250))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | 当前目录总体积: "),
            Span::styled(
                format_size(self.total_size),
                Style::default()
                    .fg(Color::Rgb(166, 227, 161))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        let header_p = Paragraph::new(header_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(69, 71, 90))),
        );
        f.render_widget(header_p, chunks[0]);

        // Main List
        let items: Vec<ListItem> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let is_cur = i == self.selected_idx;
                let icon = if node.is_dir { "📁 " } else { "📄 " };
                let bar = SpaceLens::render_bar(node.percent, 16);

                let title_style = if is_cur {
                    Style::default()
                        .fg(Color::Rgb(137, 180, 250))
                        .add_modifier(Modifier::BOLD)
                } else if node.is_dir {
                    Style::default().fg(Color::Rgb(205, 214, 244))
                } else {
                    Style::default().fg(Color::Rgb(166, 173, 200))
                };

                let bar_color = if node.percent > 40.0 {
                    Color::Rgb(243, 139, 168) // Red
                } else if node.percent > 15.0 {
                    Color::Rgb(249, 226, 175) // Yellow
                } else {
                    Color::Rgb(166, 227, 161) // Green
                };

                let line = Line::from(vec![
                    Span::raw(if is_cur { "▶ " } else { "  " }),
                    Span::raw(icon),
                    Span::styled(format!("{:<35}", node.name), title_style),
                    Span::styled(format!("{bar} "), Style::default().fg(bar_color)),
                    Span::styled(
                        format!("{:>6.1}%", node.percent),
                        Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{:>12}", format_size(node.size_bytes)),
                        Style::default().fg(Color::Rgb(249, 226, 175)),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("({} 项)", node.file_count),
                        Style::default().fg(Color::Rgb(108, 112, 134)),
                    ),
                ]);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" 目录与文件占用排行 (按体积降序) ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(137, 180, 250))),
        );

        let mut state = ListState::default();
        if !self.nodes.is_empty() {
            state.select(Some(self.selected_idx));
        }
        f.render_stateful_widget(list, chunks[1], &mut state);

        // Footer
        let footer_line = Line::from(vec![
            Span::styled(
                " 控制: ",
                Style::default()
                    .fg(Color::Rgb(137, 180, 250))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(
                "[Enter/→] 下钻进入  [Backspace/←] 返回上层  [D] 移至废纸篓  [O] 在 Finder 中打开  [Q] 退出 | ",
            ),
            Span::styled(
                &self.status_message,
                Style::default().fg(Color::Rgb(249, 226, 175)),
            ),
        ]);

        let footer_p = Paragraph::new(footer_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(69, 71, 90))),
        );
        f.render_widget(footer_p, chunks[2]);

        // Confirmation modal
        if let Some(ref target) = self.confirm_delete {
            let popup_area = centered_rect(60, 25, size);
            f.render_widget(Clear, popup_area);

            let block = Block::default()
                .title(" 确认移至废纸篓 ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(243, 139, 168)));

            let text = vec![
                Line::from(""),
                Line::from(vec![Span::raw(" 确定将以下项目安全移至系统废纸篓吗？")]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    format!(" {} ({})", target.name, format_size(target.size_bytes)),
                    Style::default()
                        .fg(Color::Rgb(243, 139, 168))
                        .add_modifier(Modifier::BOLD),
                )]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        " [Y / Enter] 确认移至废纸篓 ",
                        Style::default()
                            .bg(Color::Rgb(243, 139, 168))
                            .fg(Color::Rgb(30, 30, 46))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("     "),
                    Span::styled(
                        " [N / Esc] 取消 ",
                        Style::default()
                            .bg(Color::Rgb(69, 71, 90))
                            .fg(Color::Rgb(205, 214, 244)),
                    ),
                ]),
            ];

            let p = Paragraph::new(text)
                .block(block)
                .alignment(Alignment::Center);
            f.render_widget(p, popup_area);
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
