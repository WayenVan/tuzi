use ratatui::{Frame, layout::{Alignment, Constraint, Direction, Layout, Rect}, widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState}};

use crate::{opener::OpenPicker, theme::Theme};

pub struct OpenPopup;

impl OpenPopup {
	pub fn render(frame: &mut Frame, area: Rect, picker: &OpenPicker, theme: &Theme, popup_width: u16) {
		let width = area.width.min(popup_width);
		let height = (picker.choices.len() as u16 + 2).min(area.height);
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
			.border_style(theme.style("popup.border"));
		let items = picker.choices.iter().map(|choice| ListItem::new(format!(" {}", choice.description)));
		let list = List::new(items).block(block).highlight_symbol("› ").highlight_style(theme.style("popup.selected"));
		let mut state = ListState::default().with_selected(Some(picker.selected));
		frame.render_stateful_widget(list, popup, &mut state);
	}
}
