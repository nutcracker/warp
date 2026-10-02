//! Grid tooltips for the terminal view

use pathfinder_geometry::vector::vec2f;
use warpui::elements::{
    ChildAnchor, Dismiss, MouseStateHandle, OffsetPositioning, PositionedElementAnchor,
    PositionedElementOffsetBounds, Stack,
};
use warpui::{AppContext, Element, EventContext};

use super::{GridHighlightedLink, TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::terminal::TerminalModel;
use crate::terminal::links::directly_open_link_keybinding_string;
use crate::terminal::model::{ObfuscateSecrets, Secret};
use crate::terminal::safe_mode_settings::get_secret_obfuscation_mode;
use crate::terminal::view::SecretTooltip;
use crate::util::tooltips::{TooltipLink, TooltipRedaction};

cfg_if::cfg_if! {
    if #[cfg(feature = "local_fs")] {
        use chrono::DateTime;
        use warpui::elements::{ConstrainedBox, Container, Flex, ParentElement, Text};
        use warpui::fonts::{Properties, Weight};

        use crate::terminal::view::RichContentLink;
        use crate::terminal::view::commit_links::CommitInfo;
        use crate::util::time_format::format_approx_duration_from_now_utc;
    }
}

#[cfg(feature = "local_fs")]
const COMMIT_TOOLTIP_MAX_WIDTH: f32 = 520.;
/// Long commit bodies are cut to this many lines so the tooltip stays near the hash.
#[cfg(feature = "local_fs")]
const COMMIT_BODY_MAX_LINES: usize = 12;

struct GridTooltipLink {
    text: String,
    action: TerminalAction,
    /// Optional detail text to show after the link.
    detail: Option<String>,
    mouse_state: MouseStateHandle,
}

/// If appropriate, returns a GridTooltipLink for opening the file in warp.
/// Mutates `detail_for_default` leaving None in place if the GridTooltipLink returned is the default
/// action on "Cmd+Click" and thus should use the detail_for_default.
#[cfg(feature = "local_fs")]
fn open_in_warp_tooltip(
    path: std::path::PathBuf,
    line_and_column_num: Option<warp_util::path::LineAndColumnArg>,
    detail_for_default: &mut Option<String>,
    mouse_state: MouseStateHandle,
    app: &AppContext,
) -> Option<GridTooltipLink> {
    use settings::Setting as _;
    use warpui::SingletonEntity;

    use crate::settings::CodeSettings;
    use crate::util::file::external_editor::EditorSettings;
    use crate::util::tooltips::should_show_open_in_warp_link;

    if !should_show_open_in_warp_link(&path, app) {
        return None;
    }

    let detail = if *CodeSettings::as_ref(app).code_as_default_editor.value() {
        detail_for_default.take()
    } else {
        None
    };
    Some(GridTooltipLink {
        text: "Open in Warp".to_string(),
        action: TerminalAction::OpenCodeInWarp {
            path,
            layout: *EditorSettings::as_ref(app).open_file_layout.value(),
            line_col: line_and_column_num,
        },
        mouse_state,
        detail,
    })
}

/// Renders a commit's subject, body, author, age and full hash for the link tooltip.
#[cfg(feature = "local_fs")]
fn render_commit_details(info: &CommitInfo, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    let background = theme.tooltip_background();
    let main_color = theme.main_text_color(background.into()).into_solid();
    let sub_color = theme.sub_text_color(background.into()).into_solid();
    let font_family = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    let mut details = Flex::column().with_child(
        Text::new(info.subject.clone(), font_family, font_size + 1.)
            .with_color(main_color)
            .with_style(Properties::default().weight(Weight::Bold))
            .finish(),
    );

    if !info.body.is_empty() {
        let mut lines = info.body.lines();
        let mut body = lines
            .by_ref()
            .take(COMMIT_BODY_MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        if lines.next().is_some() {
            body.push_str("\n…");
        }
        details.add_child(
            Container::new(
                Text::new(body, font_family, font_size)
                    .with_color(main_color)
                    .finish(),
            )
            .with_margin_top(6.)
            .finish(),
        );
    }

    let age = DateTime::from_timestamp(info.timestamp, 0)
        .map(format_approx_duration_from_now_utc)
        .unwrap_or_default();
    let byline = if age.is_empty() {
        info.author.clone()
    } else {
        format!("{} · {age}", info.author)
    };
    details.add_child(
        Container::new(
            Text::new(byline, font_family, font_size)
                .with_color(sub_color)
                .finish(),
        )
        .with_margin_top(6.)
        .finish(),
    );
    details.add_child(
        Container::new(
            Text::new(
                info.full_hash.clone(),
                appearance.monospace_font_family(),
                font_size,
            )
            .with_color(sub_color)
            .finish(),
        )
        .with_margin_top(2.)
        .finish(),
    );

    ConstrainedBox::new(details.finish())
        .with_max_width(COMMIT_TOOLTIP_MAX_WIDTH)
        .finish()
}

/// Returns a GridTooltipLink for revealing the file in the platform's file explorer
/// (Finder on macOS, file manager on Linux/Windows).
#[cfg(feature = "local_fs")]
fn show_in_file_explorer_tooltip(
    path: std::path::PathBuf,
    mouse_state: MouseStateHandle,
) -> GridTooltipLink {
    let text = if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Show containing folder"
    }
    .to_string();
    GridTooltipLink {
        text,
        action: TerminalAction::ShowInFileExplorer(path),
        mouse_state,
        detail: None,
    }
}

impl TerminalView {
    fn is_commit_link(link: &GridHighlightedLink) -> bool {
        match link {
            #[cfg(feature = "local_fs")]
            GridHighlightedLink::Commit(_) => true,
            GridHighlightedLink::Url(_) | GridHighlightedLink::Hyperlink { .. } => false,
            #[cfg(feature = "local_fs")]
            GridHighlightedLink::File(_) => false,
        }
    }

    /// Renders the link and/or secrets tooltips on top of the grid
    /// Expects at least one of the two tooltips to be visible.
    // Unused variables allowed when no local filesystem as the `app` argument
    // is unused.
    #[cfg_attr(not(feature = "local_fs"), allow(unused_variables))]
    pub(super) fn render_grid_tooltip(
        &self,
        stack: &mut Stack,
        model: &TerminalModel,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        let mut element_id = "terminal_view:first_cell_in_link".to_string();
        let mut links = vec![];
        #[cfg_attr(not(feature = "local_fs"), allow(unused_mut))]
        let mut content = None;
        let mut is_agent_conversation = false;

        if let Some(open_secret_tooltip) = &self.open_secret_tool_tip {
            match open_secret_tooltip {
                SecretTooltip::Grid {
                    tooltip,
                    is_agent_mode,
                } => {
                    let handle = *tooltip;
                    is_agent_conversation = *is_agent_mode;

                    // Position the tooltip above the first cell in the secret.
                    element_id = format!(
                        "terminal_view:first_cell_in_secret_{}",
                        handle.get_inner().id()
                    );

                    if matches!(get_secret_obfuscation_mode(app), ObfuscateSecrets::Yes) {
                        let is_redacted = model
                            .secret_from_handle(tooltip)
                            .is_some_and(Secret::is_obfuscated);

                        if is_redacted {
                            links.push(GridTooltipLink {
                                text: "Reveal secret".to_string(),
                                action: TerminalAction::ToggleGridSecret {
                                    handle,
                                    show_secret: true,
                                },
                                mouse_state: self.mouse_states.toggle_secrets_tooltip.clone(),
                                detail: None,
                            });
                        } else {
                            links.push(GridTooltipLink {
                                text: "Hide secret".to_string(),
                                action: TerminalAction::ToggleGridSecret {
                                    handle,
                                    show_secret: false,
                                },
                                mouse_state: self.mouse_states.toggle_secrets_tooltip.clone(),
                                detail: None,
                            })
                        }
                    }

                    links.push(GridTooltipLink {
                        text: "Copy secret".to_string(),
                        action: TerminalAction::CopyGridSecret(handle),
                        mouse_state: self.mouse_states.copy_secrets_tooltip.clone(),
                        detail: None,
                    });
                }
                SecretTooltip::RichContent {
                    tooltip,
                    is_agent_mode,
                } => {
                    let tooltip_info = tooltip;
                    // Position the tooltip above the first cell in the secret.
                    element_id = tooltip_info.position_id.to_owned();
                    is_agent_conversation = *is_agent_mode;

                    if matches!(get_secret_obfuscation_mode(app), ObfuscateSecrets::Yes) {
                        let is_obfuscated = tooltip_info.is_obfuscated;

                        if is_obfuscated {
                            links.push(GridTooltipLink {
                                text: "Reveal secret".to_string(),
                                action: TerminalAction::ToggleRichContentSecret {
                                    rich_content_tooltip_info: tooltip_info.clone(),
                                    show_secret: true,
                                },
                                mouse_state: self.mouse_states.toggle_secrets_tooltip.clone(),
                                detail: None,
                            });
                        } else {
                            links.push(GridTooltipLink {
                                text: "Hide secret".to_string(),
                                action: TerminalAction::ToggleRichContentSecret {
                                    rich_content_tooltip_info: tooltip_info.clone(),
                                    show_secret: false,
                                },
                                mouse_state: self.mouse_states.toggle_secrets_tooltip.clone(),
                                detail: None,
                            })
                        }
                    }

                    links.push(GridTooltipLink {
                        text: "Copy secret".to_string(),
                        action: TerminalAction::CopyRichContentSecret(tooltip_info.clone()),
                        mouse_state: self.mouse_states.copy_secrets_tooltip.clone(),
                        detail: None,
                    });
                }
            }
        }

        #[cfg(feature = "local_fs")]
        if let Some(GridHighlightedLink::Commit(commit_link)) = &self.open_grid_link_tool_tip {
            let info = &commit_link.get_inner().info;
            if info.web_url.is_some() {
                let modifier = directly_open_link_keybinding_string();
                links.push(GridTooltipLink {
                    text: "Open commit".to_owned(),
                    action: TerminalAction::OpenGridLink(GridHighlightedLink::Commit(
                        commit_link.clone(),
                    )),
                    mouse_state: self.mouse_states.grid_link_tooltip.clone(),
                    detail: Some(format!("[{modifier} Click]")),
                });
            }
            links.push(GridTooltipLink {
                text: "Copy hash".to_owned(),
                action: TerminalAction::CopyCommitHash(info.full_hash.clone()),
                mouse_state: self.mouse_states.copy_commit_hash_tooltip.clone(),
                detail: None,
            });
            content = Some(render_commit_details(info, appearance));
        }

        #[cfg_attr(not(feature = "local_fs"), allow(unused_mut))]
        if let Some(link) = &self.open_grid_link_tool_tip
            && !Self::is_commit_link(link)
        {
            let mut open_in_warp = None;
            let mut show_in_file_explorer = None;
            let modifier = directly_open_link_keybinding_string();
            let mut detail = Some(format!("[{modifier} Click]"));
            #[cfg(feature = "local_fs")]
            {
                if let GridHighlightedLink::File(file_link) = link
                    && let Some(path) = file_link.get_inner().absolute_path()
                {
                    open_in_warp = open_in_warp_tooltip(
                        path.clone(),
                        file_link.get_inner().line_and_column_num,
                        &mut detail,
                        self.mouse_states.open_in_warp_tooltip.clone(),
                        app,
                    );
                    show_in_file_explorer = Some(show_in_file_explorer_tooltip(
                        path,
                        self.mouse_states.show_in_file_explorer_tooltip.clone(),
                    ));
                }
            }

            links.push(GridTooltipLink {
                // OSC 8 links hide the destination behind arbitrary visible
                // text, so show the URI itself in the tooltip; auto-detected
                // URLs already show their destination as the visible text.
                text: match link {
                    GridHighlightedLink::Hyperlink { uri, .. } => uri.clone(),
                    _ => link.tooltip_text().to_owned(),
                },
                action: TerminalAction::OpenGridLink(link.clone()),
                mouse_state: self.mouse_states.grid_link_tooltip.clone(),
                detail,
            });

            links.extend(open_in_warp);
            links.extend(show_in_file_explorer);
        }

        #[cfg_attr(not(feature = "local_fs"), allow(unused_mut))]
        if let Some(tooltip_info) = &self.open_rich_content_link_tool_tip {
            element_id = tooltip_info.position_id.to_owned();
            let mut open_in_warp = None;
            let mut show_in_file_explorer = None;
            let modifier_string = directly_open_link_keybinding_string();
            let mut detail = Some(format!("[{modifier_string} Click]"));

            #[cfg(feature = "local_fs")]
            {
                if let RichContentLink::FilePath {
                    absolute_path,
                    line_and_column_num,
                    ..
                } = &tooltip_info.link
                {
                    open_in_warp = open_in_warp_tooltip(
                        absolute_path.clone(),
                        *line_and_column_num,
                        &mut detail,
                        self.mouse_states.open_in_warp_tooltip.clone(),
                        app,
                    );
                    show_in_file_explorer = Some(show_in_file_explorer_tooltip(
                        absolute_path.clone(),
                        self.mouse_states.show_in_file_explorer_tooltip.clone(),
                    ));
                }
            }

            links.push(GridTooltipLink {
                text: tooltip_info.link.tooltip_text().to_owned(),
                action: TerminalAction::OpenRichContentLink(tooltip_info.link.clone()),
                mouse_state: self.mouse_states.rich_content_link_tooltip.clone(),
                detail,
            });

            links.extend(open_in_warp);
            links.extend(show_in_file_explorer);
        }

        let secret_redaction = get_secret_obfuscation_mode(app);

        // Get the secret level from the current tooltip
        let secret_level = self.open_secret_tool_tip.as_ref().and_then(|tooltip| {
            match tooltip {
                SecretTooltip::Grid { tooltip, .. } => {
                    // For grid secrets, get the secret level from the secret itself
                    model
                        .secret_from_handle(tooltip)
                        .map(|secret| secret.secret_level())
                }
                SecretTooltip::RichContent { tooltip, .. } => Some(tooltip.secret_level),
            }
        });

        let redaction = match (
            self.open_secret_tool_tip.is_some(),
            secret_redaction.should_redact_secret(),
            is_agent_conversation,
        ) {
            (true, true, true) => TooltipRedaction::SecretNotSentToLLMMessaging { secret_level },
            (true, true, false) => {
                TooltipRedaction::SecretWillNotBeSentToLLMMessaging { secret_level }
            }
            (_, _, _) => TooltipRedaction::NoRedaction,
        };
        stack.add_positioned_overlay_child(
            render_tooltip(links, redaction, content, appearance, app),
            OffsetPositioning::offset_from_save_position_element(
                element_id,
                // Add a small buffer between the tooltip and the top of the cell.
                vec2f(0., -2.),
                PositionedElementOffsetBounds::ParentByPosition,
                PositionedElementAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            ),
        );
    }
}

fn render_tooltip(
    tooltip_links: impl IntoIterator<Item = GridTooltipLink>,
    redaction: TooltipRedaction,
    content: Option<Box<dyn Element>>,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    // Convert GridTooltipLink to shared TooltipLink
    let shared_links = tooltip_links.into_iter().map(|link| {
        let action = link.action;
        TooltipLink {
            text: link.text,
            on_click: move |ctx: &mut EventContext| {
                ctx.dispatch_typed_action(action.clone());
            },
            detail: link.detail,
            mouse_state: link.mouse_state,
        }
    });

    let tooltip_content = crate::util::tooltips::render_tooltip_with_content(
        shared_links,
        redaction,
        content,
        appearance,
        app,
    );

    Dismiss::new(tooltip_content)
        .on_dismiss(|ctx, _app| {
            ctx.dispatch_typed_action(TerminalAction::MaybeDismissToolTip {
                from_keybinding: false,
            })
        })
        .finish()
}
