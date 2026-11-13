//! PROTOTYPE — 一次性原型，**不进 master**。
//!
//! # 这个原型回答的问题
//!
//! **"同一套界面"长什么样？** 排布参考 DeepSeek Harness 的截图：
//!
//! - 左侧：品牌 → **新会话** → 插件 / 自动化任务 → **工作区**（会话按工作区分组的树）→ 用户；
//! - 右侧：**对话优先**——会话标题栏 → 对话流（用户消息、"已思考"折叠块、助手长回答）
//!   → 底部大输入框（`+` / 工作区范围 / 模型选择 / 发送）。
//!
//! 三个宿主是**同一套界面**：web 与 desktop 是同一份 SPA；TUI 是它在终端里的渲染。
//! 本原型左边画 web 形态（HTML），右边画终端渲染（真 ratatui 输出），两边共用一份状态。
//!
//! # 怎么跑
//!
//! ```sh
//! cargo run -p uva-tui --example tui-layouts-prototype -- --render-all
//! cargo run -p uva-tui --example tui-layouts-prototype -- --interactive
//! cargo run -p uva-tui --example tui-layouts-prototype -- --emit-html docs/prototypes/shared-interface.prototype.html
//! ```
//!
//! # 界面来源
//!
//! uva 现有 SPA 的**能力**不变（11 个域：会话、对话、Agent、技能、工具、任务、计划、
//! 评测、模型、搜索、UI 扩展），但**排布**换成参考图那种对话优先：
//!
//! - `会话` 与 `对话` = 主界面（工作区树 + 对话流）；
//! - `插件` = 能力目录（工具 / 技能 / Agent / 模型）；
//! - `自动化任务` = 计划 / 任务 / 评测。

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

/// 一个会话（工作区树里的一个条目）。
struct Session {
    title: &'static str,
    /// 该会话在树里的层级：0 = 工作区，1 = 会话。
    depth: usize,
    /// 展开状态（仅工作区有意义）。
    open: bool,
    active: bool,
}

/// 侧栏条目：插件 / 自动化任务。
struct Entry {
    label: &'static str,
    icon: &'static str,
    active: bool,
}

/// 对话流里的一条消息。
enum Bubble {
    User(&'static str),
    Thinking(&'static str),
    Assistant(&'static str),
    /// 助手回答里的一行编号条目（粗体引导词 + 说明）。
    Item(&'static str, &'static str),
    Plain(&'static str),
}

struct State {
    tree: Vec<Session>,
    entries: Vec<Entry>,
    cursor: usize,
    /// 输入框里的字。
    input: String,
    /// 模态：删除确认。
    modal: Option<&'static str>,
    /// 当前是主界面还是能力目录。
    page: Page,
    /// 侧栏是否折叠成图标栏（与 SPA 的 `useSidebarCollapse` 同义）。
    collapsed: bool,
    /// 思考块是否还在流式输出：输出中展开，输出完自动折叠成一行。
    thinking_live: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Chat,
    Catalog,
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Chat => "认识 uva Harness",
            Page::Catalog => "插件",
            Page::Settings => "设置",
        }
    }
}

/// 会话里的待办（显示在输入框上方，带进度）。
const TODOS: [(&str, &str); 5] = [
    ("读仓库结构", "done"),
    ("找出可复用的插件", "done"),
    ("改写 agent-loop 为插件行", "doing"),
    ("补能力矩阵门禁", "todo"),
    ("跑一遍 make ci", "todo"),
];

/// 设置页的分区。
///
/// 定义的**唯一来源是 plugin 排布**（`.agents` 布局）；设置面只管"启用与参数"，
/// 全部记在一个 `settings.yaml`。插件入口也在这里（不再挂在对话界面）。
const SETTINGS: [(&str, &str, &str, &str); 9] = [
    ("account", "账户", "👤", "用户 · 订阅 · 凭据"),
    ("general", "通用", "⚙", "主题 · 密度 · 快捷键"),
    ("plugin", "插件", "🧩", "plugin 排布 · 启停与参数"),
    ("model", "模型", "◈", "provider 定义 · 启用"),
    ("agents", "智能体", "🤖", "内置默认 agent + plugin 排布"),
    ("skills", "技能与命令", "⌘", "plugin 排布 · 启停"),
    ("mcp", "MCP", "🔌", "plugin 排布 · 启停"),
    ("hooks", "Hooks", "🪝", "plugin 排布 · 启停"),
    ("automation", "自动化", "⏰", "plugin 排布 · 计划与模板"),
];

/// 每个分区在终端里的行（名称 + 值）。plugin 排布来的分区，值是"启用/停用"。
const SETTINGS_ROWS: [&[(&str, &str)]; 9] = [
    &[("用户", "dev"), ("订阅", "免费"), ("凭据", "3 个 provider")],
    &[("主题", "深色"), ("密度", "紧凑"), ("快捷键", "18 个")],
    &[
        ("来源", ".agents/plugins"),
        ("已启用", "1 个"),
        ("已停用", "1 个"),
    ],
    &[
        ("来源", ".agents/providers"),
        ("模型", "5 个 · 4 启用"),
        ("凭据", "2 个"),
    ],
    &[
        ("默认 agent", "内置 · 始终启用"),
        ("代码审查 agent", "已启用"),
        ("文档 agent", "已停用"),
    ],
    &[
        ("skill", "8 个 · 6 启用"),
        ("命令", "6 个 · 6 启用"),
        ("来源", ".agents/skills"),
    ],
    &[
        ("computer-use", "内置 · 开"),
        ("context7", "开"),
        ("来源", "mcp.json"),
    ],
    &[
        ("pre-commit", "开"),
        ("审计", "开"),
        ("来源", "plugin 排布"),
    ],
    &[
        ("已配置的自动化", "2 个"),
        ("执行历史", "12 条"),
        ("任务模板", "3 个"),
    ],
];

impl State {
    fn new() -> Self {
        Self {
            tree: vec![
                Session {
                    title: "默认工作区",
                    depth: 0,
                    open: true,
                    active: false,
                },
                Session {
                    title: "认识 uva Harness",
                    depth: 1,
                    open: false,
                    active: true,
                },
                Session {
                    title: "生成 Q3 数据汇总 PPT",
                    depth: 1,
                    open: false,
                    active: false,
                },
                Session {
                    title: "准备明天评审的议题清单",
                    depth: 1,
                    open: false,
                    active: false,
                },
                Session {
                    title: "给销售报表补上同比/环比列",
                    depth: 1,
                    open: false,
                    active: false,
                },
                Session {
                    title: "数据分析项目",
                    depth: 0,
                    open: false,
                    active: false,
                },
                Session {
                    title: "插件开发",
                    depth: 0,
                    open: false,
                    active: false,
                },
            ],
            entries: vec![
                Entry {
                    label: "插件",
                    icon: "🧩",
                    active: false,
                },
                Entry {
                    label: "自动化任务",
                    icon: "⏰",
                    active: false,
                },
            ],
            cursor: 0,
            input: String::new(),
            modal: None,
            page: Page::Chat,
            collapsed: false,
            thinking_live: false,
        }
    }
}

/// 对话内容（照参考图的结构：用户消息 → "已思考"折叠块 → 助手长回答）。
fn conversation() -> Vec<Bubble> {
    vec![
        Bubble::User("可以介绍一下自己吗？"),
        Bubble::Thinking("已思考"),
        Bubble::Assistant(
            "我是 uva，一个「一切皆插件」的 agent harness，内核只认识 Fiber / Effect / Service / Event。",
        ),
        Bubble::Plain(""),
        Bubble::Assistant("我可以帮你："),
        Bubble::Plain(""),
        Bubble::Item("1. 日常助手", "整理资料、汇总表格、检索文档。"),
        Bubble::Item("2. 写代码、改仓库", "理解项目、修改文件、跑测试。"),
        Bubble::Item("3. 查资料、核对信息", "给出来源与结论。"),
        Bubble::Item("4. 批量处理任务", "后台执行、定时复核。"),
        Bubble::Item("5. 安装插件、扩展能力", "用现成插件，或写自己的。"),
        Bubble::Plain(""),
        Bubble::Plain("更多事情，都可以交给我。"),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// 终端渲染
// ─────────────────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, st: &State) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());
    draw_header(frame, areas[0], st);

    let body = Layout::horizontal([
        Constraint::Length(if st.collapsed { 7 } else { 28 }),
        Constraint::Min(40),
    ])
    .split(areas[1]);
    draw_sidebar(frame, body[0], st);
    match st.page {
        Page::Chat => draw_chat(frame, body[1], st),
        Page::Catalog => draw_catalog(frame, body[1], st),
        Page::Settings => draw_settings(frame, body[1], st),
    }

    if let Some(modal) = st.modal {
        draw_modal(frame, frame.area(), modal);
    } else {
        draw_footer(frame, areas[2], st);
    }
}

/// 设置导航（Qoder 排布）：用户卡 → Ctrl+F 搜索 → 分区列表。
fn draw_settings_nav(frame: &mut Frame, area: Rect, st: &State) {
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                " 🧑 ",
                Style::default().fg(Color::Black).bg(Color::Magenta),
            ),
            Span::styled(
                " dev",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" 免费 ", Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(""),
    ];
    lines.push(Line::from(Span::styled(
        "┌ Ctrl+F 搜索 ─────────┐",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));
    for (i, (_id, label, icon, _)) in SETTINGS.iter().enumerate() {
        let selected = i == st.cursor;
        let marker = if selected {
            Span::styled(" ▸ ", Style::default().fg(Color::Cyan))
        } else {
            Span::raw("   ")
        };
        let style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            marker,
            Span::styled(
                format!("{icon} "),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(label.to_string(), style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// 用户消息气泡：右对齐 + 灰底（与 web 的气泡一致）。
fn user_bubble(text: &str, width: u16) -> Line<'static> {
    let body = format!(" {text} ");
    let used = display_width(&body) + 4;
    let pad = (width as usize).saturating_sub(used);
    Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(
            " 你 ",
            Style::default().fg(Color::DarkGray).bg(Color::Gray),
        ),
        Span::styled(body, Style::default().fg(Color::White).bg(Color::Gray)),
    ])
}

/// 折叠态：图标栏（只留图标，4~7 列宽）。
fn draw_sidebar_rail(frame: &mut Frame, area: Rect, st: &State) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "◐",
        Style::default().fg(Color::Cyan),
    )));
    lines.push(Line::from(Span::styled(
        "+",
        Style::default().fg(Color::Black).bg(Color::Cyan),
    )));
    lines.push(Line::raw(""));
    for entry in &st.entries {
        let active = entry.active;
        let style = if active {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(entry.icon.to_string(), style)));
    }
    lines.push(Line::raw(""));
    for (i, node) in st.tree.iter().enumerate() {
        if node.depth != 0 {
            continue;
        }
        let selected = i == st.cursor;
        let style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if node.open { "▾" } else { "▸" },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("🗂", style),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "🧑",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

/// 顶栏：品牌 + 会话标题 + 命令面板提示。
fn draw_header(frame: &mut Frame, area: Rect, st: &State) {
    let title = st.page.title();
    let line = Line::from(vec![
        Span::styled(
            " uva ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" HARNESS ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("  {title}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ⌘K ", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// 侧栏：品牌 / 新会话 / 插件 / 自动化任务 / 工作区树 / 用户。
///
/// 折叠时收成**图标栏**（与 SPA 的 rail 同义）：只留图标，标题靠 hover 提示。
fn draw_sidebar(frame: &mut Frame, area: Rect, st: &State) {
    if st.page == Page::Settings {
        draw_settings_nav(frame, area, st);
        return;
    }
    if st.collapsed {
        draw_sidebar_rail(frame, area, st);
        return;
    }
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        " 🕐 新会话 ",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::raw(""));

    for entry in &st.entries {
        let active = entry.active;
        let marker = if active {
            Span::styled(" ▸ ", Style::default().fg(Color::Cyan))
        } else {
            Span::raw("   ")
        };
        let style = if active {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            marker,
            Span::raw(format!("{} ", entry.icon)),
            Span::styled(entry.label.to_string(), style),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            " 工作区",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   🔍 ⇅ 📁", Style::default().fg(Color::DarkGray)),
    ]));

    for (i, node) in st.tree.iter().enumerate() {
        let selected = i == st.cursor;
        let indent = if node.depth == 0 { " " } else { "  " };
        let caret = if node.depth == 0 {
            if node.open { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let marker = if selected {
            Span::styled(" ▸ ", Style::default().fg(Color::Cyan))
        } else {
            Span::raw("   ")
        };
        let style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else if node.depth == 0 {
            Style::default().fg(Color::Gray)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(vec![
            marker,
            Span::raw(format!("{indent}{caret}")),
            Span::styled(node.title.to_string(), style),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " 🧑 dev",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" 侧栏 ")),
        area,
    );
}

/// 主区：对话流（对话优先）+ 待办进度 + prompt 建议 + 输入框。
fn draw_chat(frame: &mut Frame, area: Rect, st: &State) {
    let inner = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(5),
    ])
    .split(area);
    let mut lines: Vec<Line> = Vec::new();
    for bubble in conversation() {
        match bubble {
            Bubble::User(text) => {
                // 与 web 一致：用户消息是右对齐的气泡。
                lines.push(user_bubble(text, area.width));
            }
            Bubble::Thinking(text) => {
                if st.thinking_live {
                    // 还在输出：展开，边流边滚。
                    lines.push(Line::from(Span::styled(
                        "▾ 已思考（输出中…）",
                        Style::default().fg(Color::Cyan),
                    )));
                    lines.push(Line::from(Span::styled(
                        "  正在读仓库结构、找出可复用的插件、确认这次改动的影响面……",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    // 输出完成：自动折叠成一行。
                    lines.push(Line::from(Span::styled(
                        format!("  {text} >"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            Bubble::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
            Bubble::Item(head, body) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {head} "),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("—— {body}"),
                        Style::default().fg(Color::Gray),
                    ),
                ]));
            }
            Bubble::Plain(text) => {
                lines.push(Line::from(Span::raw(format!("  {text}"))))
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner[0],
    );

    // 待办进度（输入框上方）：有 todo 就显示进度，没有就不占地方。
    let done = TODOS.iter().filter(|(_, s)| *s == "done").count();
    let mut todo_lines: Vec<Line> = Vec::new();
    todo_lines.push(Line::from(vec![
        Span::styled(
            " 待办 ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::styled(
            format!("  {}/{} 完成", done, TODOS.len()),
            Style::default().fg(Color::Gray),
        ),
    ]));
    for (text, state) in TODOS.iter().take(3) {
        let mark = match *state {
            "done" => Span::styled("✔ ", Style::default().fg(Color::Green)),
            "doing" => Span::styled("▸ ", Style::default().fg(Color::Cyan)),
            _ => Span::styled("· ", Style::default().fg(Color::DarkGray)),
        };
        let style = if *state == "done" {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        todo_lines.push(Line::from(vec![
            mark,
            Span::styled(format!("  {text}"), style),
        ]));
    }
    frame.render_widget(
        Paragraph::new(todo_lines).block(Block::bordered().title(" 进度 ")),
        inner[1],
    );

    // 输入框：bash 式提示符（❯ + 工作区路径），**不做** prompt 建议条（D31，推迟）

    // 输入框
    let composer = vec![
        Line::from(vec![
            Span::styled(" ❯ ", Style::default().fg(Color::Green)),
            Span::styled(
                "~/workspace/uva ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                st.input.to_string(),
                Style::default().fg(Color::White),
            ),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" + ", Style::default().fg(Color::DarkGray)),
            Span::styled("  工作区内修改 ▾", Style::default().fg(Color::Gray)),
            Span::raw("                      "),
            Span::styled("model ▾", Style::default().fg(Color::Gray)),
            Span::styled(
                "  ( ↑ ) ",
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(composer).block(Block::bordered().title(" 输入 ")),
        inner[2],
    );
}

/// 设置页：吸收 Qoder 的排布（侧栏变成设置导航）+ TRAE 的分区内容。
fn draw_settings(frame: &mut Frame, area: Rect, st: &State) {
    let section = st.cursor.min(SETTINGS.len() - 1);
    let (_id, title, _icon, summary) = SETTINGS[section];
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        summary.to_string(),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));
    for (name, value) in SETTINGS_ROWS[section] {
        lines.push(Line::from(vec![
            Span::styled(pad(name, 24), Style::default().fg(Color::Gray)),
            Span::styled(value.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    if title == "自动化" {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  尚未配置自动化任务。（TRAE 风格：已配置 / 执行历史 / 任务模板 + 从模板创建）",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// 能力目录（"插件"入口）：工具 / 技能 / Agent / 模型。
fn draw_catalog(frame: &mut Frame, area: Rect, st: &State) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "插件",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "Agent 在运行中能调用的能力：工具、技能、Agent、模型。",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));
    for (i, (name, kind)) in [
        ("read", "工具"),
        ("write", "工具"),
        ("shell", "工具"),
        ("todo", "工具"),
        ("code-review", "技能"),
        ("deep-research", "技能"),
        ("默认 agent", "Agent"),
    ]
    .iter()
    .enumerate()
    {
        let selected = i == st.cursor;
        let marker = if selected {
            Span::styled(" ▸ ", Style::default().fg(Color::Cyan))
        } else {
            Span::raw("   ")
        };
        lines.push(Line::from(vec![
            marker,
            Span::styled(pad(name, 18), Style::default().fg(Color::White)),
            Span::styled(pad(kind, 6), Style::default().fg(Color::DarkGray)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, st: &State) {
    let keys = match st.page {
        Page::Chat => "j/k 移动 · enter 打开会话 · d 删除 · / 搜索 · n 新会话",
        Page::Catalog => "j/k 移动 · enter 打开 · esc 回对话",
        Page::Settings => "j/k 选分区 · enter 进入 · esc 回对话",
    };
    let line = Line::from(vec![
        Span::styled(" j/k ", Style::default().fg(Color::Cyan)),
        Span::raw("移动  "),
        Span::styled(" enter ", Style::default().fg(Color::Cyan)),
        Span::raw("打开  "),
        Span::styled(" d ", Style::default().fg(Color::Cyan)),
        Span::raw("删除  "),
        Span::styled(" 1/2 ", Style::default().fg(Color::Cyan)),
        Span::raw("切视图  "),
        Span::raw(keys),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_modal(frame: &mut Frame, area: Rect, title: &str) {
    let width = 52.min(area.width);
    let height = 7.min(area.height);
    let modal = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, modal);
    let lines = vec![
        Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::raw("  删除后不可撤销。")),
        Line::from(Span::raw("  这个会话的记录会一起消失。")),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                " y 确认 ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " n 取消 ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered().border_style(Style::default().fg(Color::Red)),
        ),
        modal,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 宽度工具
// ─────────────────────────────────────────────────────────────────────────────

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2300..=0x23FF | 0x25A0..=0x27BF | 0x2E80..=0x303E
        | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

fn pad(s: &str, width: usize) -> String {
    let used = display_width(s);
    let mut out = s.to_string();
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 三种跑法
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "--render-all" => render_all(),
        "--interactive" => interactive(),
        "--emit-html" => {
            let path = std::env::args().nth(2).unwrap_or_else(|| {
                "shared-interface.prototype.html".to_string()
            });
            emit_html(&path)?;
            println!("LAYOUT-PROTOTYPE: OK html -> {path}");
            Ok(())
        }
        _ => {
            println!("界面原型：对话优先的同一套界面（web 形态 + 终端渲染）");
            println!("  --render-all    打印画面（纯文本）");
            println!("  --interactive   交互模式（1 对话 / 2 插件目录）");
            println!("  --emit-html <p> 生成可交互的界面原型");
            println!("LAYOUT-PROTOTYPE: OK shared interface");
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 界面原型导出：左边 web 形态（照参考图画），右边终端渲染（真 ratatui 画面）
// ─────────────────────────────────────────────────────────────────────────────

fn color_css(color: Color) -> Option<&'static str> {
    match color {
        Color::Black => Some("#1c1c1c"),
        Color::Red => Some("#cc0000"),
        Color::Green => Some("#4e9a06"),
        Color::Yellow => Some("#c4a000"),
        Color::Blue => Some("#3465a4"),
        Color::Magenta => Some("#75507b"),
        Color::Cyan => Some("#06989a"),
        Color::Gray => Some("#d3d7cf"),
        Color::DarkGray => Some("#888a85"),
        Color::LightRed => Some("#ef2929"),
        Color::LightGreen => Some("#8ae234"),
        Color::LightYellow => Some("#fce94f"),
        Color::LightBlue => Some("#729fcf"),
        Color::LightMagenta => Some("#ad7fa8"),
        Color::LightCyan => Some("#34e2e2"),
        Color::White => Some("#ffffff"),
        _ => None,
    }
}

/// 一个 buffer → 一段带样式的 HTML（按样式相同的连续格合并成 span；宽字符跳过续格）。
fn frame_html(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let cell = &buffer[(x, y)];
            let (fg, bg, modifier) = (cell.fg, cell.bg, cell.modifier);
            let mut run = String::new();
            while x < area.right() {
                let cell = &buffer[(x, y)];
                if cell.fg != fg || cell.bg != bg || cell.modifier != modifier {
                    break;
                }
                let symbol = cell.symbol();
                run.push_str(symbol);
                x += 1;
                if symbol.chars().next().is_some_and(is_wide) {
                    // 宽字符占两列，续格（ratatui 置成空格）不画，否则列会错开。
                    x += 1;
                }
            }
            let mut style = String::new();
            if let Some(css) = color_css(fg) {
                style.push_str(&format!("color:{css};"));
            }
            if let Some(css) = color_css(bg) {
                style.push_str(&format!("background:{css};"));
            }
            if modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:700;");
            }
            let text = run
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            if style.is_empty() {
                out.push_str(&text);
            } else {
                out.push_str(&format!("<span style=\"{style}\">{text}</span>"));
            }
        }
        out.push('\n');
    }
    out
}

fn emit_html(path: &str) -> io::Result<()> {
    // 终端画面：每个页面 × 每个选中项 = 一帧。
    let mut frames: Vec<String> = Vec::new();
    let mut offsets: Vec<[usize; 5]> = Vec::new();
    for (page, count) in [
        (Page::Chat, 7usize),
        (Page::Catalog, 8usize),
        (Page::Settings, 5usize),
    ] {
        for collapsed in [false, true] {
            for thinking in [false, true] {
                if page != Page::Chat && thinking {
                    continue;
                }
                if page == Page::Settings && collapsed {
                    continue;
                }
                offsets.push([
                    page as usize,
                    collapsed as usize,
                    thinking as usize,
                    frames.len(),
                    count,
                ]);
                for cursor in 0..count {
                    let mut st = State::new();
                    st.page = page;
                    st.collapsed = collapsed;
                    st.thinking_live = thinking;
                    st.cursor = cursor;
                    let mut terminal = Terminal::new(
                        ratatui::backend::TestBackend::new(120, 26),
                    )
                    .map_err(io::Error::other)?;
                    terminal
                        .draw(|frame| draw(frame, &st))
                        .map_err(io::Error::other)?;
                    frames.push(frame_html(terminal.backend().buffer()));
                }
            }
        }
    }

    let sections_json = serde_json::to_string(
        &SETTINGS
            .iter()
            .map(|(id, label, icon, _)| serde_json::json!({"id": id, "label": label, "icon": icon}))
            .collect::<Vec<_>>(),
    )
    .map_err(io::Error::other)?;

    let tree_json = serde_json::to_string(
        &State::new()
            .tree
            .iter()
            .map(|n| {
                serde_json::json!({"title": n.title, "depth": n.depth, "open": n.open, "active": n.active})
            })
            .collect::<Vec<_>>(),
    )
    .map_err(io::Error::other)?;

    let page = r##"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<title>界面原型 · 对话优先的同一套界面</title>
<style>
  :root { --bg:#0e1116; --side:#171b24; --panel:#141821; --line:#272d3a; --fg:#e6e9f0;
           --muted:#8a8f9c; --accent:#4a9eda; --bubble:#232a36; }
  * { box-sizing:border-box; }
  body { margin:0; background:var(--bg); color:var(--fg);
         font:14px/1.6 -apple-system,"Segoe UI","Noto Sans CJK SC",sans-serif; }
  .wrap { max-width:1760px; margin:0 auto; padding:18px 16px 92px; }
  h1 { font-size:20px; margin:0 0 4px; }
  .sub { color:var(--muted); margin:0 0 14px; font-size:13px; max-width:110ch; }
  .proto { display:inline-block; font-size:11px; letter-spacing:.12em; text-transform:uppercase;
            color:#c4a000; border:1px solid #4a3f22; background:#241f12; border-radius:4px;
            padding:2px 7px; vertical-align:3px; margin-left:8px; }
  .panes { display:grid; grid-template-columns:1fr 1fr; gap:12px; }
  .panes.only-web, .panes.only-tui { grid-template-columns:1fr; }
  .pane { background:var(--panel); border:1px solid var(--line); border-radius:10px; overflow:hidden; }
  .pane h2 { font-size:11.5px; letter-spacing:.08em; text-transform:uppercase; color:var(--muted);
              margin:0; padding:8px 12px; border-bottom:1px solid var(--line); background:#171b24; }
  /* ── web 形态（照参考图） ───────────────────────────── */
  .app { display:flex; height:520px; }
  .side { width:236px; background:var(--side); border-right:1px solid var(--line);
           display:flex; flex-direction:column; padding:10px 8px; }
  .side.collapsed { width:58px; align-items:center; padding:10px 4px; }
  .side.collapsed .navitem, .side.collapsed .user { justify-content:center; padding:7px 0; }
  .side.collapsed .newbtn { width:34px; height:34px; padding:0; border-radius:9px; }
  .side.collapsed .tree { width:100%; }
  .railnode { display:flex; align-items:center; justify-content:center; gap:3px; padding:6px 0;
               border-radius:7px; cursor:pointer; color:#b9c1cf; }
  .railnode.on { background:#2c3a4a; color:#fff; }
  .toggle { margin-left:auto; background:none; border:0; color:var(--muted); cursor:pointer;
            font-size:13px; padding:2px 6px; border-radius:6px; }
  .toggle:hover { background:#232a36; color:var(--fg); }
  .side.collapsed .toggle { margin:0; }
  .brand { display:flex; align-items:center; gap:7px; padding:4px 6px 10px; }
  .brand .logo { font-size:16px; }
  .brand .name { font-weight:700; letter-spacing:.01em; }
  .brand .badge { font-size:9.5px; letter-spacing:.1em; border:1px solid var(--line); color:var(--muted);
                   border-radius:3px; padding:0 4px; }
  .newbtn { width:100%; text-align:center; background:#28303d; border:1px solid #333c4c; color:var(--fg);
             border-radius:8px; padding:8px 0; cursor:pointer; font:inherit; font-size:13.5px; }
  .newbtn:hover { background:#303a49; }
  .navitem { display:flex; align-items:center; gap:8px; padding:7px 8px; border-radius:7px;
              cursor:pointer; color:#cdd3de; }
  .navitem:hover { background:#202632; }
  .navitem.on { background:#2a3444; color:#fff; }
  .grouphead { display:flex; align-items:center; margin:14px 0 4px; padding:0 8px; color:#aeb6c4; }
  .grouphead .acts { margin-left:auto; color:var(--muted); letter-spacing:2px; font-size:12px; }
  .tree { overflow:auto; flex:1; }
  .node { display:flex; align-items:center; gap:6px; padding:5px 8px; border-radius:7px;
           cursor:pointer; color:#b9c1cf; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .node:hover { background:#202632; }
  .node.on { background:#2c3a4a; color:#fff; }
  .node.workspace { color:#e6e9f0; font-weight:600; }
  .node.child { padding-left:26px; }
  .node .caret { width:10px; color:var(--muted); }
  .node .wsicon { color:var(--accent); }
  .user { display:flex; align-items:center; gap:8px; padding:8px; margin-top:8px;
           border-top:1px solid var(--line); color:#b9c1cf; font-size:13px; }
  /* ── 主区 ───────────────────────────────────────────── */
  .main { flex:1; display:flex; flex-direction:column; min-width:0; }
  .topbar { display:flex; align-items:center; padding:10px 16px; border-bottom:1px solid var(--line); }
  .topbar .t { font-weight:600; }
  .topbar .r { margin-left:auto; color:var(--muted); }
  .chat { flex:1; overflow:auto; padding:18px 22px; }
  .bub { max-width:56%; margin-left:auto; background:var(--bubble); border-radius:12px;
          padding:8px 13px; margin-bottom:16px; }
  .think { color:var(--muted); font-size:13px; margin:2px 0 12px; cursor:pointer; }
  .think.live { color:var(--accent); }
  .thinkbody { color:var(--muted); border-left:2px solid var(--line); padding:4px 0 4px 10px;
                margin:6px 0 0; font-size:12.5px; }
  .answer { max-width:78ch; }
  .answer p { margin:0 0 10px; }
  .answer ol { margin:0 0 10px; padding-left:0; list-style:none; counter-reset:item; }
  .answer li { margin:0 0 8px; counter-increment:item; }
  .answer li b { font-weight:650; }
  .composer { margin:0 22px 18px; background:#181d26; border:1px solid var(--line); border-radius:12px; }
  .composer .in { padding:12px 14px; color:var(--muted); }
  .composer .row { display:flex; align-items:center; gap:10px; padding:8px 10px; }
  .composer .ico { width:26px; height:26px; border-radius:50%; background:#252c38; color:#cdd3de;
                    display:grid; place-items:center; font-size:14px; cursor:pointer; }
  .composer .scope { color:#cdd3de; font-size:13px; }
  .composer .model { margin-left:auto; color:#cdd3de; font-size:13px; }
  .composer .send { width:28px; height:28px; border-radius:50%; background:var(--accent); color:#06121d;
                     display:grid; place-items:center; font-weight:700; }
  .catalog { padding:16px 20px; }
  .catalog h3 { margin:0 0 2px; font-size:16px; }
  .catalog p { margin:0 0 12px; color:var(--muted); font-size:13px; }
  .crow { display:grid; grid-template-columns:1fr 70px; gap:10px; padding:7px 9px; border-radius:7px; cursor:pointer; }
  .crow.on { background:#2c3a4a; }
  .crow .kind { color:var(--muted); font-size:12.5px; }
  /* 待办进度 + prompt 建议（输入框上方） */
  .todo { margin:0 22px 8px; border:1px solid var(--line); border-radius:10px; background:#161a22; padding:8px 12px; }
  .todo .h { display:flex; gap:10px; align-items:center; font-size:13px; margin-bottom:5px; }
  .todo .h .tag2 { background:#2a3a52; color:#9fd6d8; font-size:11px; padding:1px 7px; border-radius:9px; }
  .todo .h .count { color:var(--muted); margin-left:auto; font-size:12px; }
  .todo .item { font-size:12.5px; color:#c3cad6; padding:1px 0; }
  .todo .item.done { color:var(--muted); }
  .chips { display:flex; gap:6px; margin:0 22px 8px; flex-wrap:wrap; }
  .chip { border:1px solid var(--line); border-radius:12px; padding:3px 10px; font-size:12.5px;
           color:#9fd6d8; cursor:pointer; }
  .chip:hover { background:#1e2a33; }
  /* 设置页 */
  .settings { display:flex; height:100%; }
  .settings .secs { width:212px; border-right:1px solid var(--line); padding:14px 8px; }
  .settings .sec { padding:7px 9px; border-radius:7px; cursor:pointer; color:#cdd3de; font-size:13.5px; }
  .settings .sec.on { background:#2a3444; color:#fff; font-weight:600; }
  .settings .body { flex:1; padding:18px 22px; overflow:auto; }
  .settings .body h3 { margin:0 0 10px; font-size:16px; }
  .settings .row { display:flex; justify-content:space-between; gap:20px; padding:8px 0;
                    border-bottom:1px solid var(--line); font-size:13.5px; max-width:640px; }
  .settings .row .v { color:var(--muted); }
  /* 吸收 TRAE / Qoder 的设置内容样式 */
  .card { border:1px solid var(--line); border-radius:10px; background:#171b23; padding:12px 14px;
          margin-bottom:12px; max-width:760px; }
  .card .ttl { font-size:14px; font-weight:600; margin-bottom:3px; }
  .card .desc { color:var(--muted); font-size:12.5px; line-height:1.5; }
  .card .ctl { float:right; margin-left:14px; }
  .toggle { width:34px; height:19px; border-radius:10px; background:#39404d; position:relative;
             display:inline-block; vertical-align:middle; cursor:pointer; }
  .toggle.on { background:#2f9e6a; }
  .toggle::after { content:''; position:absolute; top:2px; left:2px; width:15px; height:15px;
                    border-radius:50%; background:#fff; transition:.12s; }
  .toggle.on::after { left:17px; }
  .btn { border:1px solid var(--line); background:#232a36; color:var(--fg); border-radius:7px;
          padding:5px 12px; font:inherit; font-size:12.5px; cursor:pointer; }
  .btn.primary { background:#e8eaee; color:#12151b; border-color:#e8eaee; }
  .srow { display:flex; gap:12px; align-items:flex-start; padding:11px 12px; border-bottom:1px solid var(--line); }
  .srow:last-child { border-bottom:0; }
  .srow .ic { width:30px; height:30px; border-radius:8px; background:#243044; color:#8fb8e8;
               display:grid; place-items:center; font-size:15px; flex:0 0 30px; }
  .srow .nm { font-size:13.5px; font-weight:600; }
  .srow .badge { font-size:10px; letter-spacing:.06em; border:1px solid var(--line); color:var(--muted);
                  border-radius:3px; padding:0 4px; margin-left:6px; }
  .srow .ds { color:var(--muted); font-size:12.5px; margin-top:3px; line-height:1.45; }
  .srow .acts { margin-left:auto; display:flex; gap:10px; align-items:center; color:var(--muted); }
  .tabs { display:flex; gap:20px; border-bottom:1px solid var(--line); margin:14px 0 18px; }
  .tabs span { padding:8px 2px; color:var(--muted); cursor:pointer; }
  .tabs span.on { color:var(--fg); border-bottom:2px solid var(--fg); font-weight:600; }
  .empty { text-align:center; padding:60px 0; color:var(--muted); }
  .empty .big { font-size:22px; margin-bottom:10px; }
  .empty .btn { margin-top:14px; }
  .searchnav { width:100%; border:1px solid var(--line); border-radius:8px; padding:7px 11px;
                color:var(--muted); font-size:13px; margin:10px 0 4px; }
  /* ── 终端形态 ─────────────────────────────────────────── */
  .term { background:#11141a; overflow:auto; }
  pre { margin:0; padding:10px 12px; font:12.5px/1.22 "DejaVu Sans Mono","Noto Sans Mono CJK SC",monospace;
         white-space:pre; }
  .switch { position:fixed; left:50%; bottom:18px; transform:translateX(-50%); display:flex; gap:14px;
             align-items:center; background:#1b2130; border:1px solid #3b4356; border-radius:999px;
             padding:9px 16px; box-shadow:0 8px 28px rgba(0,0,0,.5); z-index:9; }
  .switch button { background:none; border:0; color:var(--fg); font-size:15px; cursor:pointer; padding:0 6px; }
  .switch .label { font-size:13px; }
  .switch .key { color:var(--accent); font-weight:700; }
  .switch .mode { display:flex; gap:4px; }
  .switch .mode button.on { background:#2a3a52; border-radius:12px; color:#fff; }
  .hint { color:var(--muted); font-size:12.5px; margin:12px 0 0; }
</style>
</head>
<body>
<div class="wrap">
  <h1>对话优先的同一套界面 <span class="proto">原型 · 一次性</span></h1>
  <p class="sub">
    左边是 <b>web 形态</b>（照 DeepSeek Harness 的排布：品牌 → 新会话 → 插件 / 自动化任务 →
    工作区树 → 用户；右侧是对话流 + 大输入框）；右边是<b>同一屏</b>的<b>终端渲染</b>（真 ratatui 输出）。
    两边共用一份状态：点树上的会话、切换入口，右侧同步变化。
  </p>
  <div class="panes" id="panes">
    <div class="pane"><h2>web 形态（HTML 画）</h2><div class="app" id="web"></div></div>
    <div class="pane"><h2>终端渲染（真 ratatui 输出）</h2><div class="term"><pre id="tui"></pre></div></div>
  </div>
  <p class="hint">
    键位：<b>1</b> 对话 · <b>2</b> 插件目录 · <b>3</b> 设置 · <b>j/k</b> 上下移动 · <b>enter</b> 打开会话 ·
    <b>d</b> 删除确认 · <b>t</b> 思考块（输出中展开 / 完成后折叠）· <b>Ctrl+B</b> 折叠侧栏。
    左侧树、入口、prompt 建议与左下角用户（设置入口）都可点。
  </p>
</div>
<div class="switch">
  <button id="prev">◀</button>
  <span class="label"><span class="key" id="pkey">1</span> <span id="pname">对话</span></span>
  <button id="next">▶</button>
  <span class="mode">
    <button data-mode="both" class="on">并排</button>
    <button data-mode="web">只看 web</button>
    <button data-mode="tui">只看终端</button>
  </span>
</div>
<script>
const FRAMES = __FRAMES__;
const OFFSETS = __OFFSETS__;
const TREE = __TREE__;
const SECTIONS = __SECTIONS__;
let page = 0, cursor = 0, collapsed = false, thinking = false;

function frameIndex() {
  const row = OFFSETS.find(r => r[0] === page && r[1] === (collapsed ? 1 : 0) && r[2] === (thinking ? 1 : 0))
          || OFFSETS.find(r => r[0] === page);
  return row[3] + Math.min(cursor, row[4] - 1);
}

function renderTui() { document.getElementById('tui').innerHTML = FRAMES[frameIndex()]; }

function sidebar() {
  if (page === 2) {
    // 设置页：侧栏换成设置导航（Qoder 排布：用户卡 → Ctrl+F 搜索 → 分区）
    const secs = SECTIONS.map((s, i) =>
      `<div class="navitem ${i === cursor ? 'on' : ''}" data-sec="${i}"><span>${s.icon}</span><span>${s.label}</span></div>`).join('');
    return `<div class="side">
      <div class="brand"><span class="logo" style="background:var(--accent);color:#06121d;border-radius:8px;padding:2px 7px">🧑</span>
        <span class="name">dev</span><span class="badge">免费</span></div>
      <div class="searchnav">🔍 Ctrl+F 搜索</div>
      ${secs}
      <div class="user">🧑 dev</div>
    </div>`;
  }
  if (collapsed) {
    const rails = TREE.map((n, i) => n.depth === 0
      ? `<div class="railnode ${i === cursor ? 'on' : ''}" data-node="${i}">
           <span class="caret">${n.open ? '▾' : '▸'}</span><span>🗂</span></div>` : '').join('');
    return `<div class="side collapsed">
      <div class="brand"><span class="logo">◐</span><button class="toggle" id="toggle" title="展开侧栏">▣</button></div>
      <button class="newbtn" title="新会话">+</button>
      <div style="height:8px"></div>
      <div style="height:10px"></div>
      <div class="tree">${rails}</div>
      <div class="user" data-page="2" title="设置">🧑</div>
    </div>`;
  }
  const nav = '';
  const nodes = TREE.map((n, i) =>
    `<div class="node ${n.depth === 0 ? 'workspace' : 'child'} ${i === cursor ? 'on' : ''}" data-node="${i}">
       <span class="caret">${n.depth === 0 ? (n.open ? '▾' : '▸') : ''}</span>
       ${n.depth === 0 ? '<span class="wsicon">🗂</span>' : ''}<span>${n.title}</span></div>`).join('');
  return `<div class="side">
    <div class="brand"><span class="logo">◐</span><span class="name">uva</span><span class="badge">HARNESS</span>
      <button class="toggle" id="toggle" title="折叠侧栏">▣</button></div>
    <button class="newbtn">🕐 新会话</button>
    <div style="height:8px"></div>
    ${nav}
    <div class="grouphead"><span>工作区</span><span class="acts">🔍 ⇅ 📁</span></div>
    <div class="tree">${nodes}</div>
    <div class="user" data-page="2" title="设置">🧑 dev</div>
  </div>`;
}

function chat() {
  return `<div class="main">
    <div class="topbar"><span class="t">认识 uva Harness</span><span class="r">▣</span></div>
    <div class="chat">
      <div class="bub">可以介绍一下自己吗？</div>
      ${thinking
        ? '<div class="think live">▾ 已思考（输出中…）<div class="thinkbody">正在读仓库结构、找出可复用的插件、确认这次改动的影响面……</div></div>'
        : '<div class="think">已思考 &gt;</div>'}
      <div class="answer">
        <p>我是 uva，一个「一切皆插件」的 agent harness，内核只认识 Fiber / Effect / Service / Event。</p>
        <p>我可以帮你：</p>
        <ol>
          <li><b>1. 日常助手</b> —— 整理资料、汇总表格、检索文档。</li>
          <li><b>2. 写代码、改仓库</b> —— 理解项目、修改文件、跑测试。</li>
          <li><b>3. 查资料、核对信息</b> —— 给出来源与结论。</li>
          <li><b>4. 批量处理任务</b> —— 后台执行、定时复核。</li>
          <li><b>5. 安装插件、扩展能力</b> —— 用现成插件，或写自己的。</li>
        </ol>
        <p>更多事情，都可以交给我。</p>
      </div>
    </div>
    <div class="todo">
      <div class="h"><span class="tag2">待办</span><span>这一轮的计划</span>
        <span class="count">2/5 完成</span></div>
      <div class="item done">✔ 读仓库结构</div>
      <div class="item done">✔ 找出可复用的插件</div>
      <div class="item">▸ 改写 agent-loop 为插件行</div>
    </div>
    <div class="chips">
      <span class="chip">总结这段代码</span><span class="chip">写个测试</span>
      <span class="chip">解释这个报错</span><span class="chip">/命令</span>
    </div>
    <div class="composer">
      <div class="in">描述你想要构建的内容，/调用指令，@文件或对话</div>
      <div class="row"><span class="ico">+</span><span class="scope">🕐 工作区内修改 ⌄</span>
        <span class="model">model ⌄</span><span class="send">↑</span></div>
    </div>
  </div>`;
}

function catalog() {
  const rows = [['read','工具'],['write','工具'],['shell','工具'],['todo','工具'],
                ['code-review','技能'],['deep-research','技能'],['默认 agent','Agent']]
    .map((r, i) => `<div class="crow ${i === cursor ? 'on' : ''}" data-row="${i}">
        <span>${r[0]}</span><span class="kind">${r[1]}</span></div>`).join('');
  return `<div class="main">
    <div class="topbar"><span class="t">插件</span><span class="r">▣</span></div>
    <div class="catalog"><h3>插件</h3>
      <p>Agent 在运行中能调用的能力：工具、技能、Agent、模型。</p>${rows}</div>
  </div>`;
}

function settings() {
  const s = SECTIONS[Math.min(cursor, SECTIONS.length - 1)];
  const source = (path, note) => `<div class="card"><div class="ttl">来源</div>
      <div class="desc">${note} 这些行由 <code>${path}</code> 装配进 profile；
        设置页是**解析后的 profile 的反推**——core 装的就是这棵树，这里只改启用与参数。
        <button class="btn" style="margin-left:8px">⟳ 重新载入</button></div></div>`;
  const row = (name, desc, on, badge) => `<div class="srow"><span class="ic">🧩</span>
      <div><div class="nm">${name}${badge ? `<span class="badge">${badge}</span>` : ''}</div>
      <div class="ds">${desc}</div></div>
      <div class="acts"><span class="toggle ${on ? 'on' : ''}"></span><span>⚙</span></div></div>`;
  const body = {
    account: `<div class="card"><div class="ctl"><span class="toggle on"></span></div>
        <div class="ttl">账户</div><div class="desc">dev · 免费版 · 凭据只存在本机</div></div>
      <div class="card"><div class="ttl">凭据</div>
        <div class="desc">配置 API key 以添加更多可用模型。</div>
        ${row('anthropic', '3 个模型可用', true)}
        ${row('openai', '已配置', true)}</div>`,
    general: `<div class="card"><div class="ttl">外观</div>
        <div class="desc">主题、密度、字号与会话气泡样式。存在本机，跨工作区保留。</div>
        <div class="row"><span>主题</span><span class="v">深色 ▾</span></div>
        <div class="row"><span>密度</span><span class="v">紧凑 ▾</span></div>
        <div class="row"><span>字号</span><span class="v">14 ▾</span></div>
        <div class="row"><span>快捷键</span><span class="v">18 个</span></div></div>
      <div class="card"><div class="ttl">settings.yaml</div>
        <div class="desc">启用与参数记在这一个文件里——它是 profile 解析链上的<b>可写一层</b>；
          写它即重新解析、重新挂载。agent 可通过内置的 settings MCP 动态读写。</div>
        <div class="row"><span>路径</span><span class="v">&lt;workspace&gt;/settings.yaml</span></div>
        <div class="row"><span>层序</span><span class="v">bundle → profile → settings → home → CLI</span></div></div>`,
    plugin: `${source('.agents/plugins', '插件')}
      <div class="card"><div class="ttl">已发现的插件</div>
        <div class="desc">plugin 排布里声明的插件；这里只决定启用与否与参数。</div>
        ${row('web-search', '联网检索 · 3 个 provider', true)}
        ${row('legacy-csv', '旧版 CSV 导入', false)}</div>`,
    model: `${source('.agents/providers', '模型与凭据')}
      <div class="card"><div class="ttl">模型</div>
        <div class="desc">provider 定义来自 plugin 排布；启用后才出现在模型选择器里。</div>
        ${row('anthropic', 'claude-sonnet / claude-opus', true)}
        ${row('openai', 'gpt-4o', true)}</div>`,
    agents: `${source('.agents/agents', '智能体')}
      <div class="card"><div class="ttl">内置</div>
        <div class="desc">整个 agent harness 靠内置的默认 agent 运转，它不可停用。</div>
        ${row('默认 agent', '内置 · 始终启用 · 整个 harness 的运转方式', true, '内置')}</div>
      <div class="card"><div class="ttl">来自 plugin 排布</div>
        <div class="desc">在默认 agent 之外附加的智能体。</div>
        ${row('代码审查 agent', 'gpt-4o · 3 个工具', true)}
        ${row('文档 agent', 'claude-sonnet · 2 个工具', false)}</div>`,
    skills: `${source('.agents/skills', '技能与命令')}
      <div class="card"><div class="ttl">技能</div>
        <div class="desc">技能与命令的正文在 plugin 排布里；这里只开关。</div>
        ${row('security-review', '代码安全扫描', true)}
        ${row('generate-mini-app', '生成小程序脚手架', false)}
        ${row('debugger', '调试辅助指令', true)}</div>`,
    mcp: `${source('mcp.json', 'MCP server')}
      <div class="card"><div class="ttl">MCP Servers</div>
        <div class="desc">server 随服务端存活；这里只开关。</div>
        ${row('computer-use', '内置 · 桌面操作与截图', true, '内置')}
        ${row('context7', '库文档检索', true)}</div>`,
    hooks: `${source('.agents/plugins', 'Hooks')}
      <div class="card"><div class="ttl">已配置的 Hooks</div>
        <div class="desc">Hook 的定义来自 plugin 排布，不再导入别处的配置文件。</div>
        ${row('pre-commit', '提交前跑检查', true)}
        ${row('审计', '记录工具调用', true)}</div>
      <div class="card"><div class="ttl">运行方式</div>
        <div class="row"><span>Hooks 命令运行方式</span><span class="v">沙箱运行 ▾</span></div>
        <div class="row"><span>运行日志</span><span class="v"><button class="btn">查看日志</button></span></div></div>`,
    automation: `<h3 style="font-size:19px;margin:0 0 4px">自动化</h3>
      <div style="color:var(--muted);margin-bottom:14px">配置和管理自动化任务，让 uva 按计划执行任务</div>
      <div style="display:flex;gap:8px"><button class="btn">手动新建</button>
        <button class="btn primary">⏱ 在对话中创建</button></div>
      <div class="tabs"><span class="on">已配置</span><span>执行历史</span><span>任务模板</span></div>
      <div class="empty"><div class="big">⏱</div><div>尚未配置自动化任务。</div>
        <button class="btn primary">从模板创建 →</button></div>
      ${source('.agents/plugins', '自动化任务与模板')}`,
  }[s.id];
  return `<div class="main">
    <div class="topbar"><span class="t">${s.label}</span><span class="r">esc 关闭</span></div>
    <div class="body" style="flex:1;padding:20px 26px;overflow:auto">${body}</div>
  </div>`;
}

function render() {
  const body = page === 0 ? chat() : page === 1 ? catalog() : settings();
  document.getElementById('web').innerHTML = sidebar() + body;
  renderTui();
  document.getElementById('pkey').textContent = String(page + 1);
  document.getElementById('pname').textContent = page === 0 ? '对话' : page === 1 ? '插件目录' : '设置';
  document.querySelectorAll('[data-node]').forEach(el => el.onclick = () => {
    cursor = Number(el.dataset.node); page = 0; render();
  });
  document.querySelectorAll('[data-row]').forEach(el => el.onclick = () => { cursor = Number(el.dataset.row); render(); });
  document.querySelectorAll('[data-sec]').forEach(el => el.onclick = () => { cursor = Number(el.dataset.sec); render(); });
  document.querySelectorAll('[data-page]').forEach(el => el.onclick = () => {
    page = Number(el.dataset.page); cursor = 0; render();
  });
  const toggle = document.getElementById('toggle');
  if (toggle) toggle.onclick = () => { collapsed = !collapsed; render(); };
  const url = new URL(location.href);
  url.searchParams.set('page', String(page + 1));
  history.replaceState(null, '', url);
}

document.getElementById('prev').onclick = () => { page = (page + 2) % 3; cursor = 0; render(); };
document.getElementById('next').onclick = () => { page = (page + 1) % 3; cursor = 0; render(); };
document.querySelectorAll('.switch .mode button').forEach(btn => btn.onclick = () => {
  document.querySelectorAll('.switch .mode button').forEach(b => b.classList.remove('on'));
  btn.classList.add('on');
  document.getElementById('panes').className = 'panes only-' + btn.dataset.mode;
});

document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && (e.key === 'b' || e.key === 'B')) {
    collapsed = !collapsed; e.preventDefault(); render(); return;
  }
  if (e.key === '1') { page = 0; cursor = 0; }
  else if (e.key === '2') { page = 1; cursor = 0; }
  else if (e.key === '3') { page = 2; cursor = 0; }
  else if (e.key === 't') { thinking = !thinking; }
  else if (e.key === 'j' || e.key === 'ArrowDown') { cursor = Math.min(cursor + 1, 9); }
  else if (e.key === 'k' || e.key === 'ArrowUp') { cursor = Math.max(cursor - 1, 0); }
  else return;
  e.preventDefault(); render();
});

const params = new URLSearchParams(location.search);
const p = Number(params.get('page'));
if (p >= 1 && p <= 3) page = p - 1;
render();
</script>
</body>
</html>
"##
        .replace("__FRAMES__", &serde_json::to_string(&frames).map_err(io::Error::other)?)
        .replace("__OFFSETS__", &serde_json::to_string(&offsets).map_err(io::Error::other)?)
        .replace("__TREE__", &tree_json)
        .replace("__SECTIONS__", &sections_json);
    std::fs::write(path, page).map_err(io::Error::other)?;
    Ok(())
}

fn render_all() -> io::Result<()> {
    for (page, label) in [(Page::Chat, "对话"), (Page::Catalog, "插件目录")]
    {
        println!("\n════ {label} ════");
        let mut st = State::new();
        st.page = page;
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(120, 26))
                .map_err(io::Error::other)?;
        terminal
            .draw(|frame| draw(frame, &st))
            .map_err(io::Error::other)?;
        print!("{}", buffer_text(terminal.backend().buffer()));
    }
    println!("LAYOUT-PROTOTYPE: OK 2 pages");
    Ok(())
}

fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut skip = 0u16;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            if let Some(first) = symbol.chars().next()
                && is_wide(first)
            {
                skip = 1;
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn interactive() -> io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(io::Error::other)?;
    let mut st = State::new();

    loop {
        terminal
            .draw(|frame| draw(frame, &st))
            .map_err(io::Error::other)?;

        if event::poll(std::time::Duration::from_millis(200))
            .map_err(io::Error::other)?
            && let Event::Key(key) = event::read().map_err(io::Error::other)?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('1') => st.page = Page::Chat,
                KeyCode::Char('2') => st.page = Page::Catalog,
                KeyCode::Char('3') => st.page = Page::Settings,
                KeyCode::Char('j') | KeyCode::Down => {
                    st.cursor = (st.cursor + 1).min(st.tree.len() - 1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    st.cursor = st.cursor.saturating_sub(1)
                }
                KeyCode::Char('d') => st.modal = Some("删除会话"),
                KeyCode::Char('t') => st.thinking_live = !st.thinking_live,
                KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Esc => {
                    st.modal = None
                }
                KeyCode::Char('b')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    st.collapsed = !st.collapsed;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
