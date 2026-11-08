use std::io::{self, stdout};
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
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::deleter::SafeDeleter;
use crate::domain::{CleanCategory, CleanItem, SafetyLevel, ScanSummary, format_size};
use crate::maintenance::SystemMaintenance;
use crate::scanners::CleanupCoordinator;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivePane {
    Sidebar,
    ItemList,
}

#[derive(Clone, PartialEq, Eq)]
enum AppMode {
    Browsing,
    ConfirmingClean,
    Cleaning,
    DoneCleaning(String),
}

pub struct CleanerApp {
    summary: ScanSummary,
    selected_sidebar_idx: usize,
    selected_item_idx: usize,
    active_pane: ActivePane,
    mode: AppMode,
    sidebar_items: Vec<SidebarEntry>,
    status_message: String,
    expanded_items: Vec<String>,
}

#[derive(Clone)]
enum SidebarEntry {
    All,
    Category(CleanCategory),
    Maintenance,
}

impl SidebarEntry {
    fn title(&self) -> &'static str {
        match self {
            Self::All => "🧹 智能扫描 (全部)",
            Self::Category(CleanCategory::SystemJunk) => "⚙️ 系统垃圾",
            Self::Category(CleanCategory::AppJunk) => "📦 应用垃圾",
            Self::Category(CleanCategory::WebJunk) => "🌐 上网垃圾",
            Self::Category(CleanCategory::DevJunk) => "💻 开发工具",
            Self::Category(CleanCategory::LargeOldFiles) => "📁 大文件与旧文件",
            Self::Category(CleanCategory::Trash) => "🗑️ 废纸篓",
            Self::Maintenance => "⚡ 系统性能优化",
        }
    }
}

impl CleanerApp {
    pub fn new(summary: ScanSummary) -> Self {
        let sidebar_items = vec![
            SidebarEntry::All,
            SidebarEntry::Category(CleanCategory::SystemJunk),
            SidebarEntry::Category(CleanCategory::AppJunk),
            SidebarEntry::Category(CleanCategory::WebJunk),
            SidebarEntry::Category(CleanCategory::DevJunk),
            SidebarEntry::Category(CleanCategory::LargeOldFiles),
            SidebarEntry::Category(CleanCategory::Trash),
            SidebarEntry::Maintenance,
        ];

        Self {
            summary,
            selected_sidebar_idx: 0,
            selected_item_idx: 0,
            active_pane: ActivePane::Sidebar,
            mode: AppMode::Browsing,
            sidebar_items,
            status_message: "就绪。按 [Space] 勾选，[Enter] 开始安全清理，[Tab] 切换焦点".into(),
            expanded_items: Vec::new(),
        }
    }

    /// Get current items visible in the right pane.
    fn current_items_mut(&mut self) -> Vec<&mut CleanItem> {
        let entry = self.sidebar_items[self.selected_sidebar_idx].clone();
        match entry {
            SidebarEntry::All => {
                let mut all = Vec::new();
                for g in &mut self.summary.groups {
                    for it in &mut g.items {
                        all.push(it);
                    }
                }
                all
            }
            SidebarEntry::Category(cat) => {
                if let Some(g) = self.summary.get_group_mut(cat) {
                    g.items.iter_mut().collect()
                } else {
                    Vec::new()
                }
            }
            SidebarEntry::Maintenance => Vec::new(),
        }
    }

    fn current_items(&self) -> Vec<&CleanItem> {
        let entry = self.sidebar_items[self.selected_sidebar_idx].clone();
        match entry {
            SidebarEntry::All => {
                let mut all = Vec::new();
                for g in &self.summary.groups {
                    for it in &g.items {
                        all.push(it);
                    }
                }
                all
            }
            SidebarEntry::Category(cat) => {
                if let Some(g) = self.summary.get_group(cat) {
                    g.items.iter().collect()
                } else {
                    Vec::new()
                }
            }
            SidebarEntry::Maintenance => Vec::new(),
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
                        match &self.mode {
                            AppMode::Browsing => match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                KeyCode::Tab => {
                                    self.active_pane = match self.active_pane {
                                        ActivePane::Sidebar => ActivePane::ItemList,
                                        ActivePane::ItemList => ActivePane::Sidebar,
                                    };
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    self.navigate_up();
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    self.navigate_down();
                                }
                                KeyCode::Char(' ') => {
                                    self.toggle_selection();
                                }
                                KeyCode::Char('e') => {
                                    self.toggle_expand();
                                }
                                KeyCode::Char('r') => {
                                    self.rescan();
                                }
                                KeyCode::Enter => {
                                    let is_maint = matches!(
                                        self.sidebar_items[self.selected_sidebar_idx],
                                        SidebarEntry::Maintenance
                                    );
                                    if !is_maint {
                                        if self.summary.selected_size > 0 {
                                            self.mode = AppMode::ConfirmingClean;
                                        } else {
                                            self.status_message =
                                                "请先使用空格勾选待清理项目！".into();
                                        }
                                    }
                                }
                                KeyCode::Char('1') => {
                                    if matches!(
                                        self.sidebar_items[self.selected_sidebar_idx],
                                        SidebarEntry::Maintenance
                                    ) {
                                        match SystemMaintenance::free_ram() {
                                            Ok(msg) => self.status_message = msg,
                                            Err(e) => self.status_message = format!("错误: {e}"),
                                        }
                                    }
                                }
                                KeyCode::Char('2') => {
                                    if matches!(
                                        self.sidebar_items[self.selected_sidebar_idx],
                                        SidebarEntry::Maintenance
                                    ) {
                                        match SystemMaintenance::flush_dns() {
                                            Ok(msg) => self.status_message = msg,
                                            Err(e) => self.status_message = format!("错误: {e}"),
                                        }
                                    }
                                }
                                KeyCode::Char('3') => {
                                    if matches!(
                                        self.sidebar_items[self.selected_sidebar_idx],
                                        SidebarEntry::Maintenance
                                    ) {
                                        match SystemMaintenance::rebuild_spotlight(None) {
                                            Ok(msg) => self.status_message = msg,
                                            Err(e) => self.status_message = format!("错误: {e}"),
                                        }
                                    }
                                }
                                _ => {}
                            },
                            AppMode::ConfirmingClean => match key.code {
                                KeyCode::Char('y') | KeyCode::Enter => {
                                    self.execute_clean();
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    self.mode = AppMode::Browsing;
                                }
                                _ => {}
                            },
                            AppMode::DoneCleaning(_) => {
                                if key.code == KeyCode::Enter || key.code == KeyCode::Esc {
                                    self.mode = AppMode::Browsing;
                                    self.rescan();
                                }
                            }
                            AppMode::Cleaning => {}
                        }
                    }
                }
            }
        }
    }

    fn navigate_up(&mut self) {
        match self.active_pane {
            ActivePane::Sidebar => {
                if self.selected_sidebar_idx > 0 {
                    self.selected_sidebar_idx -= 1;
                    self.selected_item_idx = 0;
                }
            }
            ActivePane::ItemList => {
                if self.selected_item_idx > 0 {
                    self.selected_item_idx -= 1;
                }
            }
        }
    }

    fn navigate_down(&mut self) {
        match self.active_pane {
            ActivePane::Sidebar => {
                if self.selected_sidebar_idx + 1 < self.sidebar_items.len() {
                    self.selected_sidebar_idx += 1;
                    self.selected_item_idx = 0;
                }
            }
            ActivePane::ItemList => {
                let len = self.current_items().len();
                if len > 0 && self.selected_item_idx + 1 < len {
                    self.selected_item_idx += 1;
                }
            }
        }
    }

    fn toggle_selection(&mut self) {
        let idx = self.selected_item_idx;
        let mut items = self.current_items_mut();
        if idx < items.len() {
            let item = &mut items[idx];
            item.selected = !item.selected;
            for sub in &mut item.subitems {
                sub.selected = item.selected;
            }
        }
        self.summary.recalculate();
    }

    fn toggle_expand(&mut self) {
        let idx = self.selected_item_idx;
        let items = self.current_items();
        if idx < items.len() {
            let id = items[idx].id.clone();
            if let Some(pos) = self.expanded_items.iter().position(|x| x == &id) {
                self.expanded_items.remove(pos);
            } else {
                self.expanded_items.push(id);
            }
        }
    }

    fn rescan(&mut self) {
        self.status_message = "正在重新扫描系统各模块...".into();
        self.summary = CleanupCoordinator::scan_all();
        self.status_message = format!(
            "扫描完成！找到可清理数据共计 {}",
            format_size(self.summary.total_size)
        );
    }

    fn execute_clean(&mut self) {
        self.mode = AppMode::Cleaning;

        let mut to_clean = Vec::new();
        for g in &self.summary.groups {
            for it in &g.items {
                if it.selected {
                    to_clean.push(it.clone());
                }
            }
        }

        let report = SafeDeleter::execute(&to_clean, false, false, |_item, _cur, _tot| {});

        let msg = format!(
            "清理成功！已将 {} 项移动至废纸篓，共释放空间 {}",
            report.cleaned_items,
            format_size(report.reclaimed_bytes)
        );

        self.mode = AppMode::DoneCleaning(msg);
    }

    fn ui(&mut self, f: &mut Frame) {
        let size = f.area();

        // 3-row layout: Header, Body, Footer
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(10),   // Main View
                Constraint::Length(3), // Footer status bar
            ])
            .split(size);

        self.render_header(f, chunks[0]);
        self.render_body(f, chunks[1]);
        self.render_footer(f, chunks[2]);

        // Render modal dialogs if needed
        if let AppMode::ConfirmingClean = self.mode {
            self.render_confirm_modal(f, size);
        } else if let AppMode::DoneCleaning(ref msg) = self.mode {
            self.render_done_modal(f, size, msg);
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let title_line = Line::from(vec![
            Span::styled(
                " EVERYFILES MAC CLEANER ",
                Style::default()
                    .bg(Color::Rgb(137, 180, 250))
                    .fg(Color::Rgb(30, 30, 46))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  CleanMyMac 极速版  "),
            Span::styled(
                format!("总发现: {} ", format_size(self.summary.total_size)),
                Style::default().fg(Color::Rgb(249, 226, 175)),
            ),
            Span::raw(" | "),
            Span::styled(
                format!("已选中: {}", format_size(self.summary.selected_size)),
                Style::default()
                    .fg(Color::Rgb(166, 227, 161))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(69, 71, 90)));

        let paragraph = Paragraph::new(title_line)
            .block(block)
            .alignment(Alignment::Left);
        f.render_widget(paragraph, area);
    }

    fn render_body(&mut self, f: &mut Frame, area: Rect) {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(40)])
            .split(area);

        self.render_sidebar(f, h_chunks[0]);
        self.render_right_pane(f, h_chunks[1]);
    }

    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let is_focused = self.active_pane == ActivePane::Sidebar;
        let border_color = if is_focused {
            Color::Rgb(137, 180, 250)
        } else {
            Color::Rgb(69, 71, 90)
        };

        let items: Vec<ListItem> = self
            .sidebar_items
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let size_str = match entry {
                    SidebarEntry::All => format_size(self.summary.total_size),
                    SidebarEntry::Category(cat) => self
                        .summary
                        .get_group(*cat)
                        .map(|g| format_size(g.total_size))
                        .unwrap_or_else(|| "0 B".into()),
                    SidebarEntry::Maintenance => "工具".into(),
                };

                let selected = i == self.selected_sidebar_idx;
                let prefix = if selected { "▶ " } else { "  " };

                let style = if selected {
                    Style::default()
                        .fg(Color::Rgb(137, 180, 250))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(205, 214, 244))
                };

                let line = Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(format!("{:<15}", entry.title()), style),
                    Span::styled(
                        format!("{:>8}", size_str),
                        Style::default().fg(Color::Rgb(127, 132, 156)),
                    ),
                ]);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" 清理分类 ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color)),
        );

        let mut state = ListState::default();
        state.select(Some(self.selected_sidebar_idx));
        f.render_stateful_widget(list, area, &mut state);
    }

    fn render_right_pane(&self, f: &mut Frame, area: Rect) {
        let is_maint = matches!(
            self.sidebar_items[self.selected_sidebar_idx],
            SidebarEntry::Maintenance
        );

        if is_maint {
            self.render_maintenance_pane(f, area);
            return;
        }

        let is_focused = self.active_pane == ActivePane::ItemList;
        let border_color = if is_focused {
            Color::Rgb(137, 180, 250)
        } else {
            Color::Rgb(69, 71, 90)
        };

        let items = self.current_items();
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .flat_map(|(i, item)| {
                let is_cur = is_focused && i == self.selected_item_idx;
                let check = if item.selected { "[x]" } else { "[ ]" };
                let check_style = if item.selected {
                    Style::default()
                        .fg(Color::Rgb(166, 227, 161))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(108, 112, 134))
                };

                let title_style = if is_cur {
                    Style::default()
                        .fg(Color::Rgb(137, 180, 250))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(205, 214, 244))
                };

                let badge_style = match item.safety {
                    SafetyLevel::Clean => Style::default().fg(Color::Rgb(147, 153, 178)),
                    SafetyLevel::Suggested => Style::default().fg(Color::Rgb(166, 227, 161)),
                    SafetyLevel::Caution => Style::default().fg(Color::Rgb(249, 226, 175)),
                };

                let is_expanded = self.expanded_items.contains(&item.id);
                let expand_icon = if item.subitems.is_empty() {
                    "  "
                } else if is_expanded {
                    "▼ "
                } else {
                    "▶ "
                };

                let mut rows = vec![ListItem::new(Line::from(vec![
                    Span::styled(format!("{expand_icon}{check} "), check_style),
                    Span::styled(format!("{:<20}", item.title), title_style),
                    Span::styled(format!(" {:<8} ", item.safety.label()), badge_style),
                    Span::styled(
                        format!(" {:>10}", format_size(item.size_bytes)),
                        Style::default().fg(Color::Rgb(249, 226, 175)),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        &item.description,
                        Style::default().fg(Color::Rgb(108, 112, 134)),
                    ),
                ]))];

                if is_expanded {
                    for sub in &item.subitems {
                        let sub_check = if sub.selected { "[x]" } else { "[ ]" };
                        rows.push(ListItem::new(Line::from(vec![
                            Span::raw("      "),
                            Span::styled(
                                format!("{sub_check} "),
                                if sub.selected {
                                    Style::default().fg(Color::Rgb(166, 227, 161))
                                } else {
                                    Style::default().fg(Color::Rgb(108, 112, 134))
                                },
                            ),
                            Span::styled(
                                format!("{:<30}", sub.name),
                                Style::default().fg(Color::Rgb(186, 194, 222)),
                            ),
                            Span::styled(
                                format!("{:>10}", format_size(sub.size_bytes)),
                                Style::default().fg(Color::Rgb(127, 132, 156)),
                            ),
                        ])));
                    }
                }

                rows
            })
            .collect();

        let list = List::new(list_items).block(
            Block::default()
                .title(" 清理项详情 (按 [E] 展开/折叠，[Space] 勾选) ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color)),
        );

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.selected_item_idx));
        }
        f.render_stateful_widget(list, area, &mut state);
    }

    fn render_maintenance_pane(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" 系统性能维护工具 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(137, 180, 250)));

        let text = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " [1] 释放 RAM 内存 (purge) ",
                    Style::default()
                        .bg(Color::Rgb(49, 50, 68))
                        .fg(Color::Rgb(137, 180, 250))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - 清理系统级非活跃磁盘缓存，立即释放可用物理内存"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " [2] 刷新系统 DNS 缓存 ",
                    Style::default()
                        .bg(Color::Rgb(49, 50, 68))
                        .fg(Color::Rgb(166, 227, 161))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - 重置 dscacheutil 与 mDNSResponder，解决网络解析异常与网页打不开"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " [3] 重建 Spotlight 聚焦搜索索引 ",
                    Style::default()
                        .bg(Color::Rgb(49, 50, 68))
                        .fg(Color::Rgb(249, 226, 175))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - 执行 mdutil -E，解决系统全局搜索卡顿或结果不准"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "按键盘快捷键 [1]、[2] 或 [3] 即可立即执行对应维护任务。",
                Style::default().fg(Color::Rgb(127, 132, 156)),
            )),
        ];

        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let text = Line::from(vec![
            Span::styled(
                " 快捷键: ",
                Style::default()
                    .fg(Color::Rgb(137, 180, 250))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(
                "[Tab] 分栏  [↑/↓] 选择  [Space] 勾选  [E] 展开  [R] 重新扫描  [Enter] 确认清理  [Q] 退出 | ",
            ),
            Span::styled(
                &self.status_message,
                Style::default().fg(Color::Rgb(249, 226, 175)),
            ),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(69, 71, 90)));

        let paragraph = Paragraph::new(text).block(block);
        f.render_widget(paragraph, area);
    }

    fn render_confirm_modal(&self, f: &mut Frame, area: Rect) {
        let popup_area = centered_rect(60, 30, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" 安全清理确认 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(249, 226, 175)));

        let text = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw(" 即将清理选中的 "),
                Span::styled(
                    format_size(self.summary.selected_size),
                    Style::default()
                        .fg(Color::Rgb(166, 227, 161))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" 临时与缓存数据。"),
            ]),
            Line::from(""),
            Line::from(" 默认安全机制：文件将被安全移动至 macOS 系统废纸篓 (~/.Trash)"),
            Line::from(" 如有误删随时可在废纸篓中右键「放回原处」。"),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " [Y / Enter] 确认安全清理 ",
                    Style::default()
                        .bg(Color::Rgb(166, 227, 161))
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

    fn render_done_modal(&self, f: &mut Frame, area: Rect, msg: &str) {
        let popup_area = centered_rect(60, 25, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" 清理完成 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(166, 227, 161)));

        let text = vec![
            Line::from(""),
            Line::from(Span::styled(
                "✨ macOS 清理完毕！",
                Style::default()
                    .fg(Color::Rgb(166, 227, 161))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(msg),
            Line::from(""),
            Line::from(Span::styled(
                " 按 [Enter] 返回 ",
                Style::default()
                    .bg(Color::Rgb(137, 180, 250))
                    .fg(Color::Rgb(30, 30, 46))
                    .add_modifier(Modifier::BOLD),
            )),
        ];

        let p = Paragraph::new(text)
            .block(block)
            .alignment(Alignment::Center);
        f.render_widget(p, popup_area);
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
