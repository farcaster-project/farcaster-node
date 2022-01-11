// Rust-cli-example
//
// Written in 2021 by
//     mario <mario@zupzup.org>
//
// Addapted in 2022 by
//     danastafarai <work@alindro.ch>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use crossterm::{
    event::{self, Event as CEvent, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{
        Block, BorderType, Borders, Paragraph, Tabs,
    },
    Terminal,
};

enum Event<I> {
    Input(I),
    Tick,
}

#[derive(Copy, Clone, Debug)]
enum MenuItem {
    Home,
    Maker,
    Taker,
    Config
}

impl From<MenuItem> for usize {
    fn from(input: MenuItem) -> usize {
        match input {
            MenuItem::Home => 0,
            MenuItem::Maker => 1,
            MenuItem::Taker => 2,
            MenuItem::Config => 3,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode().expect("can run in raw mode");

    let (tx, rx) = mpsc::channel();
    let tick_rate = Duration::from_millis(200);
    thread::spawn(move || {
        let mut last_tick = Instant::now();
        loop {
            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(timeout).expect("poll works") {
                if let CEvent::Key(key) = event::read().expect("can read events") {
                    tx.send(Event::Input(key)).expect("can send events");
                }
            }

            if last_tick.elapsed() >= tick_rate {
                if let Ok(_) = tx.send(Event::Tick) {
                    last_tick = Instant::now();
                }
            }
        }
    });

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let menu_titles = vec!["1: HOME", "2: MAKER", "3: TAKER", "4: CONFIG", "5: QUIT"];
    let mut active_menu_item = MenuItem::Home;

    loop {
        terminal.draw(|rect| {
            let size = rect.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints(
                    [
                        Constraint::Length(3),
                        Constraint::Min(2),
                        Constraint::Length(3),
                    ]
                    .as_ref(),
                )
                .split(size);

            let copyright = Paragraph::new("farcaster-node-TUI 2022 - all rights reserved")
                .style(Style::default().fg(Color::LightCyan))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .style(Style::default().fg(Color::White))
                        .title("Copyright")
                        .border_type(BorderType::Plain),
                );

            let menu = menu_titles
                .iter()
                .map(|t| {
                    let (first, rest) = t.split_at(1);
                    Spans::from(vec![
                        Span::styled(
                            first,
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::UNDERLINED),
                        ),
                        Span::styled(rest, Style::default().fg(Color::White)),
                    ])
                })
                .collect();

            let tabs = Tabs::new(menu)
                .select(active_menu_item.into())
                .block(Block::default().title("MeNu").borders(Borders::ALL))
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().fg(Color::Yellow))
                .divider(Span::raw("|"));

            rect.render_widget(tabs, chunks[0]);
            match active_menu_item {
                MenuItem::Home => rect.render_widget(render_home(), chunks[1]),
                MenuItem::Maker => rect.render_widget(render_maker(), chunks[1]),
                MenuItem::Taker => rect.render_widget(render_taker(), chunks[1]),
                MenuItem::Config => rect.render_widget(render_config(), chunks[1]),
            }
            rect.render_widget(copyright, chunks[2]);
        })?;

        match rx.recv()? {
            Event::Input(event) => match event.code {
                KeyCode::Char('Q') => {
                    disable_raw_mode()?;
                    terminal.show_cursor()?;
                    terminal.clear()?;
                    break;
                }
                KeyCode::Char('q') => {
                    disable_raw_mode()?;
                    terminal.show_cursor()?;
                    terminal.clear()?;
                    break;
                }
                KeyCode::Char('5') => {
                    disable_raw_mode()?;
                    terminal.show_cursor()?;
                    terminal.clear()?;
                    break;
                }
                KeyCode::Char('1') => active_menu_item = MenuItem::Home,
                KeyCode::Char('2') => active_menu_item = MenuItem::Maker,
                KeyCode::Char('3') => active_menu_item = MenuItem::Taker,
                KeyCode::Char('4') => active_menu_item = MenuItem::Config,
                _ => {}
            },
            Event::Tick => {}
        }
    }

    Ok(())
}

// A unterground superfancy logo

fn render_home<'a>() -> Paragraph<'a> {
    let home = Paragraph::new(vec![
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBMMMMMMMMMMMBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBMMMMMBBBBBBMM",Style::default().fg(Color::Rgb(242, 104, 34)),), Span::styled("RRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),), Span::styled("MMBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBMMM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMMBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("M",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBMMMMMMMM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("BBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMMMMMMMBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(242, 104, 34)),),
                         Span::styled("RRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MMMMMMMMMMMMMM",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),)]),
        Spans::from(vec![Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("MBBBBBBBBBBBBBBBBBBBBBBBBM",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("Welcome to farcaster-node TUI")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("Press anykey and may the force be with you!")]),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::White))
            .title("HoMe")
            .border_type(BorderType::Plain),
    );
    home
}

fn render_maker<'a>() -> Paragraph<'a> {
    let home = Paragraph::new(vec![
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("            MAKER           ",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("Press anykey and may the force be with you!")]),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::White))
            .title("MaKeR")
            .border_type(BorderType::Plain),
    );
    home
}

fn render_taker<'a>() -> Paragraph<'a> {
    let home = Paragraph::new(vec![
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("            TAKER           ",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("Press anykey and may the force be with you!")]),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::White))
            .title("TaKeR")
            .border_type(BorderType::Plain),
    );
    home
}

fn render_config<'a>() -> Paragraph<'a> {
    let home = Paragraph::new(vec![
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(242, 104, 34)),)]),
        Spans::from(vec![Span::styled("RRRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),),
                         Span::styled("            CONFIG          ",Style::default().fg(Color::Rgb(77, 77, 77)),),
                         Span::styled("RRRRRRRRRRRRRRRRRRR",Style::default().fg(Color::Rgb(254, 254, 254)),)]),
        Spans::from(vec![Span::styled("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",Style::default().fg(Color::Rgb(77, 77, 77)),)]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("")]),
        Spans::from(vec![Span::raw("Press anykey and may the force be with you!")]),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::White))
            .title("CoNfIg")
            .border_type(BorderType::Plain),
    );
    home
}
