// SPDX-License-Identifier: GPL-3.0-or-later
//! [`VirtualGrid`]: a row-virtualised grid of fixed-size cells (poster grids with tens of
//! thousands of items). Only visible rows are built; Up/Down move by one row *by index*
//! (so the focus can reach rows that are not rendered yet), Left/Right move geometrically
//! inside the visible rows; PageUp/PageDown move by a page.

use eframe::egui::{self, Id, Rect, ScrollArea, Sense, Ui, Vec2, vec2};

use crate::focus::{self, Axis, Dir};

#[derive(Debug, Clone, Copy, Default)]
struct GridState {
    focused: Option<usize>,
    offset: f32,
    viewport: f32,
}

/// A virtualised grid.
///
/// ```ignore
/// let cols = Layout::grid_columns(width, poster_w);
/// VirtualGrid::new(Id::unique("movies.grid"), vec2(poster_w, PosterCard::height(poster_w)), movies.len())
///     .gap(vec2(16.0, 24.0))
///     .show(ui, |i| Id::unique(("movies.poster", movies[i].stream_id)), |ui, i, rect| {
///         let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
///         PosterCard::new(Id::unique(("movies.poster", movies[i].stream_id)), &img, &title, rect.width()).show(&mut child);
///     });
/// ```
/// Cells must register with the focus engine using `item_id(i)` (the grid is the scope).
#[derive(Debug)]
pub struct VirtualGrid {
    id: Id,
    cell: Vec2,
    gap: Vec2,
    len: usize,
    padding: Vec2,
    scroll_to: Option<usize>,
}

impl VirtualGrid {
    /// A grid of `len` cells of size `cell` (the grid id is also its focus scope).
    pub fn new(id: Id, cell: Vec2, len: usize) -> Self {
        Self {
            id,
            cell,
            gap: vec2(16.0, 24.0),
            len,
            padding: vec2(16.0, 16.0),
            scroll_to: None,
        }
    }

    /// Gaps between columns and rows.
    #[must_use]
    pub fn gap(mut self, gap: Vec2) -> Self {
        self.gap = gap;
        self
    }

    /// Inner padding (paint allowance for the focus lift and ring).
    #[must_use]
    pub fn padding(mut self, p: Vec2) -> Self {
        self.padding = p;
        self
    }

    /// Scrolls so cell `i` is visible this frame.
    #[must_use]
    pub fn scroll_to(mut self, i: Option<usize>) -> Self {
        self.scroll_to = i;
        self
    }

    /// Columns that fit `width`.
    pub fn columns_for(&self, width: f32) -> usize {
        (((width - 2.0 * self.padding.x + self.gap.x) / (self.cell.x + self.gap.x)).floor()
            as usize)
            .max(1)
    }

    /// Adds the grid, filling the available rect.
    pub fn show(
        self,
        ui: &mut Ui,
        item_id: impl Fn(usize) -> Id,
        mut add_cell: impl FnMut(&mut Ui, usize, Rect),
    ) {
        let ctx = ui.ctx().clone();
        let state_id = self.id.with("grid-state");
        let mut state: GridState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
        let cols = self.columns_for(ui.available_width());
        let rows = self.len.div_ceil(cols);
        let stride = self.cell.y + self.gap.y;
        let mut target = self.scroll_to.map(|i| i / cols);

        if self.len > 0
            && let Some(dir) = focus::take_nav(&ctx, self.id, Axis::Vertical)
        {
            let cur = state.focused.unwrap_or(0).min(self.len - 1);
            let next = match dir {
                Dir::Up => cur.checked_sub(cols),
                Dir::Down if cur + cols < self.len => Some(cur + cols),
                // Down from a partial last row: go to the last item when a row exists below.
                Dir::Down if cur / cols + 1 < rows => Some(self.len - 1),
                _ => None,
            };
            match next {
                Some(n) => {
                    focus::request(&ctx, item_id(n));
                    state.focused = Some(n);
                    target = Some(n / cols);
                }
                None => focus::give_back_nav(&ctx, dir),
            }
        }
        if focus::is_in_scope(&ctx, self.id) && !focus::text_input_focused(&ctx) && self.len > 0 {
            let page_rows = ((state.viewport / stride).floor() as usize).max(1);
            let (up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                )
            });
            let cur = state.focused.unwrap_or(0);
            let next = if up {
                Some(cur.saturating_sub(page_rows * cols))
            } else if down {
                Some((cur + page_rows * cols).min(self.len - 1))
            } else {
                None
            };
            if let Some(n) = next {
                focus::request(&ctx, item_id(n));
                state.focused = Some(n);
                target = Some(n / cols);
            }
        }

        let mut area = ScrollArea::vertical()
            .id_salt(self.id.with("grid-scroll"))
            .auto_shrink([false, false])
            .content_margin(egui::Margin {
                left: self.padding.x as i8,
                right: self.padding.x as i8,
                top: self.padding.y as i8,
                bottom: self.padding.y as i8,
            });
        if let Some(row) = target {
            let top = row as f32 * stride;
            let bottom = top + self.cell.y + self.padding.y * 2.0;
            let view = state.viewport.max(stride);
            let mut off = state.offset;
            if top < off {
                off = top;
            } else if bottom > off + view {
                off = bottom - view;
            }
            if (off - state.offset).abs() > 0.5 {
                area = area.vertical_scroll_offset(off.max(0.0));
            }
        }
        let output = focus::scope(ui, self.id, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            area.show_rows(ui, stride, rows, |ui, range| {
                for row in range {
                    let (row_rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), stride), Sense::hover());
                    for col in 0..cols {
                        let i = row * cols + col;
                        if i >= self.len {
                            break;
                        }
                        let min = row_rect.min + vec2(col as f32 * (self.cell.x + self.gap.x), 0.0);
                        add_cell(ui, i, Rect::from_min_size(min, self.cell));
                        if focus::is_focused(ui.ctx(), item_id(i)) {
                            state.focused = Some(i);
                        }
                    }
                }
            })
        });
        focus::forward_scroll(ui);
        state.offset = output.state.clamped_offset().y;
        state.viewport = output.inner_rect.height();
        ctx.data_mut(|d| d.insert_temp(state_id, state));
    }
}
