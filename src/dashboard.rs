//! Read-only local terminal dashboard. It never acquires a native writer or
//! exposes credentials; the OS account that owns the data directory is trusted.
use crate::{model::Agent, registry::Registry, teams::MemberConfig};
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;
use sqlx::Row;
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REFRESH: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OpenTarget {
    /// Open the exact released session in its configured native CLI.
    Auto,
    /// Open the exact released session in its configured native CLI.
    Cli,
    /// Reserved until Codex Desktop documents a stable external session-opening contract.
    CodexDesktop,
    /// Claude Desktop keeps separate history; transfer requires `/desktop` from an active CLI.
    ClaudeDesktop,
}

#[derive(Clone, Default)]
struct Overview {
    teams: Vec<TeamView>,
    runs: Vec<RunView>,
}

#[derive(Clone)]
struct TeamView {
    name: String,
    agents: usize,
}

#[derive(Clone)]
struct RunView {
    id: String,
    team_id: String,
    team_name: String,
    state: String,
    turns: i64,
    max_turns: i64,
    messages: i64,
    max_messages: i64,
    deadline: i64,
    error: Option<String>,
}

#[derive(Clone, Default)]
struct Details {
    agents: Vec<AgentView>,
    assignments: Vec<AssignmentView>,
    decisions: Vec<DecisionView>,
    messages: Vec<MessageView>,
}

#[derive(Clone)]
struct AgentView {
    id: String,
    name: String,
    role: String,
    provider: Option<String>,
    model: Option<String>,
    turn_state: Option<String>,
    native_id: Option<String>,
}

#[derive(Clone)]
struct AssignmentView {
    assignee: String,
    state: String,
    objective: String,
    turns_left: i64,
    messages_left: i64,
}

#[derive(Clone)]
struct DecisionView {
    state: String,
    blocking: bool,
    question: String,
}

#[derive(Clone)]
struct MessageView {
    seq: i64,
    from: String,
    to: String,
    body: String,
    delivered: bool,
}

struct App {
    data_dir: PathBuf,
    registry: Option<Registry>,
    overview: Overview,
    details: Details,
    run_index: usize,
    agent_filter: Option<usize>,
    last_refresh: Instant,
    last_error: Option<String>,
    run_area: Rect,
    agent_area: Rect,
    open_requested: Option<(String, String)>,
    pinned_run: Option<String>,
    run_list_state: ListState,
    agent_list_state: ListState,
}

struct MouseCapture;

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
}

impl App {
    async fn new(data_dir: &Path, preferred_run: Option<&str>) -> Result<Self> {
        let database = data_dir.join("registry.sqlite3");
        let registry = if database.is_file() {
            Some(Registry::open_read_only(&database).await?)
        } else {
            None
        };
        let mut app = Self {
            data_dir: data_dir.to_owned(),
            registry,
            overview: Overview::default(),
            details: Details::default(),
            run_index: 0,
            agent_filter: None,
            last_refresh: Instant::now(),
            last_error: None,
            run_area: Rect::default(),
            agent_area: Rect::default(),
            open_requested: None,
            pinned_run: preferred_run.map(str::to_owned),
            run_list_state: ListState::default(),
            agent_list_state: ListState::default(),
        };
        app.refresh().await;
        if let Some(run_id) = preferred_run {
            app.run_index = app
                .overview
                .runs
                .iter()
                .position(|run| run.id == run_id)
                .ok_or_else(|| anyhow::anyhow!("run not found in local dashboard"))?;
            app.run_list_state.select(Some(app.run_index));
            app.refresh().await;
        }
        Ok(app)
    }

    fn selected_run(&self) -> Option<&RunView> {
        self.overview.runs.get(self.run_index)
    }

    fn selected_agent(&self) -> Option<&AgentView> {
        self.agent_filter.and_then(|i| self.details.agents.get(i))
    }

    async fn refresh(&mut self) {
        let Some(registry) = &self.registry else {
            self.last_refresh = Instant::now();
            return;
        };
        let selected = self
            .selected_run()
            .map(|run| run.id.clone())
            .or_else(|| self.pinned_run.clone());
        match load_overview(registry, selected.as_deref()).await {
            Ok(overview) => {
                self.overview = overview;
                self.run_index = selected
                    .and_then(|id| self.overview.runs.iter().position(|r| r.id == id))
                    .unwrap_or(self.run_index)
                    .min(self.overview.runs.len().saturating_sub(1));
                self.run_list_state
                    .select(if self.overview.runs.is_empty() {
                        None
                    } else {
                        Some(self.run_index)
                    });
                let details = match self.selected_run() {
                    Some(run) => load_details(registry, run).await,
                    None => Ok(Details::default()),
                };
                match details {
                    Ok(details) => {
                        self.details = details;
                        if self
                            .agent_filter
                            .is_some_and(|i| i >= self.details.agents.len())
                        {
                            self.agent_filter = None;
                        }
                        self.agent_list_state.select(self.agent_filter);
                        self.last_error = None;
                    }
                    Err(error) => self.last_error = Some(error.to_string()),
                }
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
        self.last_refresh = Instant::now();
    }

    async fn move_run(&mut self, delta: isize) {
        if self.overview.runs.is_empty() {
            return;
        }
        self.run_index = self
            .run_index
            .saturating_add_signed(delta)
            .min(self.overview.runs.len() - 1);
        self.agent_filter = None;
        self.run_list_state.select(Some(self.run_index));
        self.agent_list_state.select(None);
        self.refresh().await;
    }

    async fn select_run(&mut self, index: usize) {
        if index < self.overview.runs.len() && index != self.run_index {
            self.run_index = index;
            self.agent_filter = None;
            self.run_list_state.select(Some(self.run_index));
            self.agent_list_state.select(None);
            self.refresh().await;
        }
    }

    fn cycle_agent(&mut self, backwards: bool) {
        if self.details.agents.is_empty() {
            self.agent_filter = None;
            return;
        }
        self.agent_filter = match (self.agent_filter, backwards) {
            (None, false) => Some(0),
            (Some(0), true) => None,
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.details.agents.len() => Some(i + 1),
            _ => None,
        };
        self.agent_list_state.select(self.agent_filter);
    }
}

pub async fn run(data_dir: &Path, preferred_run: Option<&str>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("the dashboard requires an interactive terminal; use agentisan --help for commands");
    }
    let mut app = App::new(data_dir, preferred_run).await?;
    let mut terminal = ratatui::try_init().context("cannot initialize terminal dashboard")?;
    if let Err(error) = execute!(std::io::stdout(), EnableMouseCapture) {
        ratatui::restore();
        return Err(error.into());
    }
    let mouse = MouseCapture;
    let result = run_loop(&mut terminal, &mut app).await;
    drop(mouse);
    ratatui::restore();
    if let Some(registry) = app.registry.clone() {
        registry.close().await;
    }
    result?;
    if let Some((run_id, agent_id)) = app.open_requested {
        open_native(data_dir, &run_id, &agent_id, OpenTarget::Cli).await?;
    }
    Ok(())
}

async fn run_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        let wait = REFRESH.saturating_sub(app.last_refresh.elapsed());
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Up | KeyCode::Char('k') => app.move_run(-1).await,
                    KeyCode::Down | KeyCode::Char('j') => app.move_run(1).await,
                    KeyCode::Tab => app.cycle_agent(false),
                    KeyCode::BackTab => app.cycle_agent(true),
                    KeyCode::Char('r') => app.refresh().await,
                    KeyCode::Char('o') => {
                        if let (Some(run), Some(agent)) = (app.selected_run(), app.selected_agent())
                        {
                            app.open_requested = Some((run.id.clone(), agent.id.clone()));
                            return Ok(());
                        }
                    }
                    _ => {}
                },
                Event::Mouse(mouse)
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
                {
                    let run_area = app.run_area;
                    if mouse.column > run_area.x
                        && mouse.column < run_area.x + run_area.width
                        && mouse.row > run_area.y
                        && mouse.row < run_area.y + run_area.height
                    {
                        if let Some(index) = clicked_index(
                            run_area,
                            mouse.row,
                            app.run_list_state.offset(),
                            app.overview.runs.len(),
                        ) {
                            app.select_run(index).await;
                        }
                        continue;
                    }
                    let area = app.agent_area;
                    if mouse.column > area.x
                        && mouse.column < area.x + area.width
                        && mouse.row > area.y
                        && mouse.row < area.y + area.height
                        && let Some(index) = clicked_index(
                            area,
                            mouse.row,
                            app.agent_list_state.offset(),
                            app.details.agents.len(),
                        )
                    {
                        app.agent_filter = Some(index);
                        app.agent_list_state.select(Some(index));
                    }
                }
                _ => {}
            }
        }
        if app.last_refresh.elapsed() >= REFRESH {
            app.refresh().await;
        }
    }
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    render_header(frame, app, header);
    if app.registry.is_none() {
        render_onboarding(frame, app, body);
    } else {
        render_body(frame, app, body);
    }
    let footer_text =
        " ↑/↓ runs  •  click/Tab agents  •  o open released chat  •  r refresh  •  q quit";
    frame.render_widget(
        Paragraph::new(footer_text).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let status = app
        .selected_run()
        .map(|run| format!("{} • {}", run.team_name, run.state.to_uppercase()))
        .unwrap_or_else(|| "NO RUN SELECTED".into());
    let line = Line::from(vec![
        Span::styled(
            " AGENTISAN ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(status, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  •  READ ONLY"),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn render_onboarding(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let text = vec![
        Line::from(Span::styled(
            "No Agentisan registry found",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("Data directory: {}", app.data_dir.display())),
        Line::from(""),
        Line::from("Create a fixture registry:"),
        Line::from("  agentisan init --fixture examples/registry.json"),
        Line::from(""),
        Line::from("Or create a managed team:"),
        Line::from("  agentisan teams create --config .agentisan/team.json"),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().title(" Welcome ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_body(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(area);
    let [runs, agents] =
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).areas(left);
    let [work, messages] =
        Layout::vertical([Constraint::Length(9), Constraint::Min(8)]).areas(right);
    app.run_area = runs;
    render_runs(frame, app, runs);
    app.agent_area = agents;
    render_agents(frame, app, agents);
    render_work(frame, app, work);
    render_messages(frame, app, messages);
}

fn render_runs(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let items: Vec<ListItem<'static>> = if app.overview.runs.is_empty() {
        app.overview
            .teams
            .iter()
            .map(|team| ListItem::new(format!("◇ {}  •  {} agents", team.name, team.agents)))
            .collect()
    } else {
        app.overview
            .runs
            .iter()
            .map(|run| {
                let color = state_color(&run.state);
                ListItem::new(Line::from(vec![
                    Span::styled("● ", Style::default().fg(color)),
                    Span::styled(
                        run.team_name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  •  {}  •  {}", run.state, short_id(&run.id))),
                ]))
            })
            .collect()
    };
    let title = format!(" Runs ({}) ", app.overview.runs.len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, &mut app.run_list_state);
}

fn render_agents(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let items: Vec<ListItem<'static>> = app
        .details
        .agents
        .iter()
        .map(|agent| {
            let active = agent.turn_state.as_deref() == Some("running");
            let marker = if active { "●" } else { "○" };
            let provider = agent
                .provider
                .as_deref()
                .map(|p| format!("{p}/{}", agent.model.as_deref().unwrap_or("default")))
                .unwrap_or_else(|| "fixture".into());
            let native = agent
                .native_id
                .as_deref()
                .map(short_id)
                .unwrap_or("unbound");
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default().fg(if active {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(
                    agent.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}  •  {provider}  •  {native}", agent.role)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().title(" Agents ").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::Blue));
    frame.render_stateful_widget(list, area, &mut app.agent_list_state);
}

fn render_work(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if let Some(run) = app.selected_run() {
        let remaining = run.deadline - unix_now();
        lines.push(Line::from(vec![
            Span::styled("RUN  ", Style::default().fg(Color::Cyan)),
            Span::raw(format!(
                "turns {}/{}  messages {}/{}  deadline {}s",
                run.turns,
                run.max_turns,
                run.messages,
                run.max_messages,
                remaining.max(0)
            )),
        ]));
        if let Some(error) = &run.error {
            lines.push(Line::from(Span::styled(
                truncate(error, 100),
                Style::default().fg(Color::Red),
            )));
        }
    }
    if let Some(error) = &app.last_error {
        lines.push(Line::from(Span::styled(
            format!("REFRESH  {}", truncate(error, 90)),
            Style::default().fg(Color::Red),
        )));
    }
    if let Some(agent) = app.selected_agent() {
        lines.push(Line::from(format!(
            "AGENT  {}  •  {}  •  {}",
            agent.name,
            agent
                .provider
                .as_deref()
                .map(|provider| format!(
                    "{provider}/{}",
                    agent.model.as_deref().unwrap_or("default")
                ))
                .unwrap_or_else(|| "fixture".into()),
            agent
                .native_id
                .as_deref()
                .map(short_id)
                .unwrap_or("unbound")
        )));
    }
    for assignment in &app.details.assignments {
        lines.push(Line::from(format!(
            "ASSIGN  {} [{}]  t:{} m:{}  {}",
            agent_label(app, &assignment.assignee),
            assignment.state,
            assignment.turns_left,
            assignment.messages_left,
            truncate(&assignment.objective, 72)
        )));
    }
    for decision in &app.details.decisions {
        lines.push(Line::from(format!(
            "DECIDE  {}{}  {}",
            decision.state,
            if decision.blocking { " / blocking" } else { "" },
            truncate(&decision.question, 78)
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from("No assignment or decision records"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Work ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_messages(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let filter = app.selected_agent().map(|agent| agent.id.as_str());
    let mut lines: Vec<Line<'_>> = app
        .details
        .messages
        .iter()
        .filter(|message| filter.is_none_or(|id| message.from == id || message.to == id))
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            let route_color = if message.from == "human" {
                Color::Yellow
            } else {
                Color::Cyan
            };
            Line::from(vec![
                Span::styled(
                    format!(
                        "{:>3}  {} → {}",
                        message.seq,
                        agent_label(app, &message.from),
                        agent_label(app, &message.to)
                    ),
                    Style::default()
                        .fg(route_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if message.delivered {
                        "  ✓  "
                    } else {
                        "  …  "
                    },
                    Style::default().fg(if message.delivered {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
                Span::raw(truncate(&compact_body(&message.body), 100)),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::from("No messages for this view"));
    }
    let title = app
        .selected_agent()
        .map(|a| format!(" Communication • {} ", a.name))
        .unwrap_or_else(|| " Communication • all agents ".into());
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

async fn load_overview(registry: &Registry, include_run: Option<&str>) -> Result<Overview> {
    let team_rows = sqlx::query("SELECT payload FROM teams ORDER BY id")
        .fetch_all(&registry.pool)
        .await?;
    let mut teams = Vec::with_capacity(team_rows.len());
    for row in team_rows {
        let team: crate::model::Team = serde_json::from_str(row.get("payload"))?;
        let agents: i64 = sqlx::query_scalar("SELECT count(*) FROM agents WHERE team_id=?")
            .bind(team.id.as_str())
            .fetch_one(&registry.pool)
            .await?;
        teams.push(TeamView {
            name: team.name,
            agents: agents as usize,
        });
    }
    let rows = sqlx::query(
        "SELECT r.id,r.team_id,r.state,r.turns,r.max_turns,r.max_messages,r.deadline,r.error,t.payload,
         (SELECT count(*) FROM messages m WHERE m.run_id=r.id AND m.staged_turn IS NULL) AS messages
         FROM runs r JOIN teams t ON t.id=r.team_id ORDER BY r.created_at DESC,r.rowid DESC LIMIT 100",
    )
    .fetch_all(&registry.pool)
    .await?;
    let mut runs = rows.iter().map(decode_run).collect::<Result<Vec<_>>>()?;
    if let Some(run_id) = include_run
        && !runs.iter().any(|run| run.id == run_id)
    {
        let row = sqlx::query(
            "SELECT r.id,r.team_id,r.state,r.turns,r.max_turns,r.max_messages,r.deadline,r.error,t.payload,
             (SELECT count(*) FROM messages m WHERE m.run_id=r.id AND m.staged_turn IS NULL) AS messages
             FROM runs r JOIN teams t ON t.id=r.team_id WHERE r.id=?",
        )
        .bind(run_id)
        .fetch_optional(&registry.pool)
        .await?;
        if let Some(row) = row {
            runs.push(decode_run(&row)?);
        }
    }
    Ok(Overview { teams, runs })
}

fn decode_run(row: &sqlx::sqlite::SqliteRow) -> Result<RunView> {
    let team: crate::model::Team = serde_json::from_str(row.get("payload"))?;
    Ok(RunView {
        id: row.get("id"),
        team_id: row.get("team_id"),
        team_name: team.name,
        state: row.get("state"),
        turns: row.get("turns"),
        max_turns: row.get("max_turns"),
        messages: row.get("messages"),
        max_messages: row.get("max_messages"),
        deadline: row.get("deadline"),
        error: row.get("error"),
    })
}

async fn load_details(registry: &Registry, run: &RunView) -> Result<Details> {
    let rows=sqlx::query("SELECT a.payload,m.config,(SELECT state FROM turns WHERE run_id=? AND agent_id=a.id ORDER BY rowid DESC LIMIT 1) AS turn_state,(SELECT native_id FROM turns WHERE run_id=? AND agent_id=a.id AND native_id IS NOT NULL ORDER BY rowid DESC LIMIT 1) AS native_id FROM agents a LEFT JOIN team_members m ON m.agent_id=a.id WHERE a.team_id=? ORDER BY a.id")
        .bind(&run.id).bind(&run.id).bind(&run.team_id).fetch_all(&registry.pool).await?;
    let mut agents = rows
        .iter()
        .map(|row| {
            let agent: Agent = serde_json::from_str(row.get("payload"))?;
            let member: Option<MemberConfig> = row
                .get::<Option<String>, _>("config")
                .map(|value| serde_json::from_str(&value))
                .transpose()?;
            Ok(AgentView {
                id: agent.id.0,
                name: agent.name,
                role: match agent.role {
                    crate::model::AgentRole::Lead => "lead".into(),
                    crate::model::AgentRole::Worker => "worker".into(),
                },
                provider: member.as_ref().map(|m| m.provider.as_str().to_owned()),
                model: member.map(|m| m.model),
                turn_state: row.get("turn_state"),
                native_id: row.get("native_id"),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    agents.sort_by(|a, b| {
        let rank = |agent: &AgentView| if agent.role == "lead" { 0 } else { 1 };
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    let rows=sqlx::query("SELECT assignee,state,objective,turn_budget-turns_used AS turns_left,message_budget-messages_used AS messages_left FROM assignments WHERE run_id=? AND staged_turn IS NULL ORDER BY created_at,id").bind(&run.id).fetch_all(&registry.pool).await?;
    let assignments = rows
        .iter()
        .map(|row| AssignmentView {
            assignee: row.get("assignee"),
            state: row.get("state"),
            objective: row.get("objective"),
            turns_left: row.get("turns_left"),
            messages_left: row.get("messages_left"),
        })
        .collect();
    let rows=sqlx::query("SELECT state,blocking,question FROM decisions WHERE run_id=? AND staged_turn IS NULL ORDER BY created_at,id").bind(&run.id).fetch_all(&registry.pool).await?;
    let decisions = rows
        .iter()
        .map(|row| DecisionView {
            state: row.get("state"),
            blocking: row.get("blocking"),
            question: row.get("question"),
        })
        .collect();
    let rows=sqlx::query("SELECT seq,sender,recipient,body,delivered_turn FROM (SELECT seq,sender,recipient,body,delivered_turn FROM messages WHERE run_id=? AND staged_turn IS NULL ORDER BY seq DESC LIMIT 200) ORDER BY seq").bind(&run.id).fetch_all(&registry.pool).await?;
    let messages = rows
        .iter()
        .map(|row| MessageView {
            seq: row.get("seq"),
            from: row.get("sender"),
            to: row.get("recipient"),
            body: row.get("body"),
            delivered: row.get::<Option<String>, _>("delivered_turn").is_some(),
        })
        .collect();
    Ok(Details {
        agents,
        assignments,
        decisions,
        messages,
    })
}

/// Open a released native session without guessing from recency or a working directory.
/// Desktop targets fail closed until their providers expose a stable exact-session contract.
pub async fn open_native(
    data_dir: &Path,
    run_id: &str,
    agent_id: &str,
    target: OpenTarget,
) -> Result<()> {
    let database = data_dir.join("registry.sqlite3");
    if !database.is_file() {
        bail!("Agentisan registry not found at {}", database.display());
    }
    let registry = Registry::open(&database).await?;
    let row=sqlx::query("SELECT r.state,m.config,(SELECT native_id FROM turns WHERE run_id=r.id AND agent_id=m.agent_id AND native_id IS NOT NULL ORDER BY rowid DESC LIMIT 1) AS native_id FROM runs r JOIN team_members m ON m.agent_id=? JOIN agents a ON a.id=m.agent_id AND a.team_id=r.team_id WHERE r.id=?")
        .bind(agent_id).bind(run_id).fetch_optional(&registry.pool).await?
        .ok_or_else(||anyhow::anyhow!("run agent not found"))?;
    let state: String = row.get("state");
    if matches!(
        state.as_str(),
        "queued" | "running" | "completing" | "stalled" | "interrupted"
    ) {
        registry.close().await;
        bail!(
            "run {run_id} is {state}; inspect it in the dashboard until Agentisan releases the native writer"
        );
    }
    let member: MemberConfig = serde_json::from_str(row.get("config"))?;
    let native_id: String = row
        .get::<Option<String>, _>("native_id")
        .ok_or_else(|| anyhow::anyhow!("agent has no recorded native session"))?;
    registry.close().await;

    match target {
        OpenTarget::CodexDesktop => {
            bail!(
                "Codex Desktop has no documented external exact-thread opening contract; use --target cli"
            )
        }
        OpenTarget::ClaudeDesktop => {
            bail!(
                "Claude Desktop keeps separate session history; use --target cli, then /desktop when an interactive transfer is appropriate"
            )
        }
        OpenTarget::Auto | OpenTarget::Cli => {}
    }
    let mut command = std::process::Command::new(&member.executable);
    match member.provider {
        crate::teams::Provider::Codex => {
            command.arg("resume").arg(&native_id);
        }
        crate::teams::Provider::Claude => {
            command.arg("--resume").arg(&native_id);
        }
    }
    let status = command
        .status()
        .with_context(|| format!("cannot open native CLI for {agent_id}"))?;
    if !status.success() {
        bail!("native CLI exited without opening the recorded session");
    }
    Ok(())
}

fn state_color(state: &str) -> Color {
    match state {
        "running" | "completing" => Color::Green,
        "queued" | "stalled" | "interrupted" => Color::Yellow,
        "completed" => Color::Cyan,
        "failed" | "exhausted" => Color::Red,
        _ => Color::DarkGray,
    }
}

fn clicked_index(area: Rect, row: u16, offset: usize, len: usize) -> Option<usize> {
    if row <= area.y || row >= area.y + area.height {
        return None;
    }
    let index = offset + usize::from(row - area.y - 1);
    (index < len).then_some(index)
}

fn short_id(value: &str) -> &str {
    value
        .rsplit('_')
        .next()
        .unwrap_or(value)
        .get(..8)
        .unwrap_or(value)
}

fn agent_label<'a>(app: &'a App, id: &'a str) -> &'a str {
    if id == "human" {
        "Human"
    } else {
        app.details
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.name.as_str())
            .unwrap_or(id)
    }
}

fn compact_body(body: &str) -> String {
    let value = serde_json::from_str::<Value>(body).ok();
    let text = match value {
        Some(Value::Object(map)) => {
            let kind = map.get("kind").and_then(Value::as_str).unwrap_or("event");
            let detail = map
                .get("objective")
                .or_else(|| map.get("question"))
                .or_else(|| map.get("choice"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("{kind} {detail}")
        }
        Some(Value::String(value)) => value,
        Some(value) => value.to_string(),
        None => body.to_owned(),
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_bodies_are_compact_and_bounded() {
        assert_eq!(
            compact_body(r#"{"kind":"assignment","objective":"Review the durable protocol"}"#),
            "assignment Review the durable protocol"
        );
        assert_eq!(compact_body("hello\n  peer"), "hello peer");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn clicks_include_the_persisted_scroll_offset() {
        let area = Rect::new(0, 10, 30, 6);
        assert_eq!(clicked_index(area, 11, 15, 30), Some(15));
        assert_eq!(clicked_index(area, 15, 15, 30), Some(19));
        assert_eq!(clicked_index(area, 10, 15, 30), None);
    }

    #[tokio::test]
    async fn exact_old_run_is_included_beyond_the_recent_window() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("registry.sqlite3");
        let registry = Registry::open(&database).await.unwrap();
        let group = crate::model::Group {
            id: "dashboard_group".into(),
            name: "Dashboard".into(),
        };
        let team = crate::model::Team {
            id: "dashboard_team".into(),
            group_id: "dashboard_group".into(),
            name: "Dashboard team".into(),
        };
        sqlx::query("INSERT INTO groups(id,payload) VALUES(?,?)")
            .bind(group.id.as_str())
            .bind(serde_json::to_string(&group).unwrap())
            .execute(&registry.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO teams(id,group_id,payload) VALUES(?,?,?)")
            .bind(team.id.as_str())
            .bind(team.group_id.as_str())
            .bind(serde_json::to_string(&team).unwrap())
            .execute(&registry.pool)
            .await
            .unwrap();
        for index in 0..101 {
            sqlx::query("INSERT INTO runs(id,team_id,lead_id,state,objective,turns,max_turns,max_messages,deadline,turn_timeout,created_at) VALUES(?,?,'lead','completed','test',0,1,64,9999999999,120,?)")
                .bind(format!("run_{index}"))
                .bind(team.id.as_str())
                .bind(index)
                .execute(&registry.pool)
                .await
                .unwrap();
        }
        let overview = load_overview(&registry, Some("run_0")).await.unwrap();
        assert_eq!(overview.runs.len(), 101);
        assert!(overview.runs.iter().any(|run| run.id == "run_0"));
        registry.close().await;
    }
}
