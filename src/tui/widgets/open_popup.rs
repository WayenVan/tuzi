use ratatui::{Frame, layout::{Alignment, Constraint, Direction, Layout, Rect}, style::{Color, Modifier, Style}, widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState}};

use crate::opener::{OpenMode, OpenPicker};

pub struct OpenPopup;

impl OpenPopup {
	pub fn render(frame: &mut Frame, area: Rect, picker: &OpenPicker) {
		let width = area.width.min(52);
		let height = (OpenMode::ALL.len() as u16 + 2).min(area.height);
		let [popup] = Layout::default()
			.direction(Direction::Vertical)
			.constraints([Constraint::Length(height)])
			.flex(ratatui::layout::Flex::Center)
			.areas(area);
		let [popup] = Layout::default()
			.direction(Direction::Horizontal)
			.constraints([Constraint::Length(width)])
			.flex(ratatui::layout::Flex::Center)
			.areas(popup);

		frame.render_widget(Clear, popup);
		let block = Block::new()
			.title(" Open with ")
			.title_alignment(Alignment::Center)
			.borders(Borders::ALL)
			.border_type(BorderType::Rounded)
			.border_style(Style::new().fg(Color::Blue));
		let items = OpenMode::ALL.into_iter().map(|mode| ListItem::new(format!(" {}", mode.label())));
		let list = List::new(items).block(block).highlight_symbol("› ").highlight_style(Style::new().fg(Color::LightCyan).add_modifier(Modifier::BOLD));
		let mut state = ListState::default().with_selected(Some(picker.selected));
		frame.render_stateful_widget(list, popup, &mut state);
	}
}
