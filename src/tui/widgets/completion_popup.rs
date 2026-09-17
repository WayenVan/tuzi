use ratatui::{Frame, layout::Rect, style::{Modifier, Style}, widgets::{Block, Clear, List, ListItem, ListState}};

pub struct CompletionPopup;

impl CompletionPopup {
	pub fn render(frame: &mut Frame, area: Rect, anchor: Rect, candidates: &[String], selected: usize, command: bool, max_items: usize) {
		if candidates.is_empty() {
			return;
		}

		let height = (candidates.len().min(max_items) as u16 + 2).min(area.height);
		let y = if anchor.bottom().saturating_add(height) <= area.bottom() {
			anchor.bottom()
		} else {
			anchor.y.saturating_sub(height)
		};
		let rect = Rect::new(anchor.x, y, anchor.width, height);
		let items = candidates.iter().map(|name| ListItem::new(if command { name.clone() } else { format!("{name}{}", std::path::MAIN_SEPARATOR) }));
		let list = List::new(items)
			.block(Block::bordered().title(if command { " Commands " } else { " Directories " }))
			.highlight_style(Style::new().add_modifier(Modifier::REVERSED));
		let mut state = ListState::default().with_selected(Some(selected));
		frame.render_widget(Clear, rect);
		frame.render_stateful_widget(list, rect, &mut state);
	}
}
