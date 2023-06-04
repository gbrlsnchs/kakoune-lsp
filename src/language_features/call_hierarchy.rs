use std::collections::HashMap;

use crate::context::*;
use crate::position::*;
use crate::types::*;
use crate::util::*;
use itertools::Itertools;
use lsp_types::{request::*, *};
use serde::Deserialize;

pub fn call_hierarchy_prepare(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = CallHierarchyParams::deserialize(params)
        .expect("Params should follow CallHierarchyParams structure");
    let position = get_lsp_position(&meta.buffile, &params.position, ctx).unwrap();
    let uri = Url::from_file_path(&meta.buffile).unwrap();
    let prepare_params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(uri),
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };

    ctx.call::<CallHierarchyPrepare, _>(
        meta,
        RequestParams::All(vec![prepare_params]),
        move |ctx: &mut Context, meta, results| {
            request_call_hierarchy(meta, ctx, params.incoming_or_outgoing, results);
        },
    )
}

fn request_call_hierarchy(
    meta: EditorMeta,
    ctx: &mut Context,
    incoming_or_outgoing: bool,
    results: Vec<(LanguageId, Option<Vec<CallHierarchyItem>>)>,
) {
    let result = results
        .into_iter()
        .find(|(_, response)| response.is_some())
        .and_then(|(language_id, item)| Some((language_id, item.unwrap())));

    // TODO Can we get multiple items here?
    let (language_id, item) = match result
        .and_then(|(language_id, r)| r.into_iter().next().and_then(|v| Some((language_id, v))))
    {
        Some(item) => item,
        None => return,
    };

    if incoming_or_outgoing {
        let params = {
            let mut m = HashMap::with_capacity(1);
            m.insert(
                language_id,
                vec![CallHierarchyIncomingCallsParams {
                    item: item.clone(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                }],
            );
            m
        };

        ctx.call::<CallHierarchyIncomingCalls, _>(
            meta,
            RequestParams::Each(params),
            move |ctx: &mut Context, meta, results| {
                if let Some(result) = results.first() {
                    format_call_hierarchy_calls(meta, ctx, incoming_or_outgoing, &item, result);
                }
            },
        );
    } else {
        let params = {
            let mut m = HashMap::with_capacity(1);
            m.insert(
                language_id,
                vec![CallHierarchyOutgoingCallsParams {
                    item: item.clone(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                }],
            );
            m
        };

        ctx.call::<CallHierarchyOutgoingCalls, _>(
            meta,
            RequestParams::Each(params),
            move |ctx: &mut Context, meta, results| {
                if let Some(result) = results.first() {
                    format_call_hierarchy_calls(meta, ctx, incoming_or_outgoing, &item, result);
                }
            },
        );
    }
}

fn format_location(
    meta: &EditorMeta,
    ctx: &mut Context,
    root_path: &str,
    uri: &Url,
    position: Position,
    prefix: &str,
    suffix: &str,
) -> String {
    let filename = uri.to_file_path().unwrap();
    let filename = short_file_path(filename.to_str().unwrap(), root_path);
    let position = get_kakoune_position_with_fallback(&meta.buffile, position, ctx);
    format!(
        "{}{}:{}:{}: {}\n",
        prefix, filename, position.line, position.column, suffix,
    )
}

trait CallHierarchyCall<'a> {
    fn caller_or_callee(&self) -> &CallHierarchyItem;
    fn caller(&'a self, other: &'a CallHierarchyItem) -> &'a CallHierarchyItem;
    fn callsites(&self) -> &Vec<Range>;
}

impl<'a> CallHierarchyCall<'a> for CallHierarchyIncomingCall {
    fn caller_or_callee(&self) -> &CallHierarchyItem {
        &self.from
    }
    fn caller(&'a self, _callee: &'a CallHierarchyItem) -> &'a CallHierarchyItem {
        &self.from
    }
    fn callsites(&self) -> &Vec<Range> {
        &self.from_ranges
    }
}

impl<'a> CallHierarchyCall<'a> for CallHierarchyOutgoingCall {
    fn caller_or_callee(&self) -> &CallHierarchyItem {
        &self.to
    }
    fn caller(&'a self, caller: &'a CallHierarchyItem) -> &'a CallHierarchyItem {
        caller
    }
    fn callsites(&self) -> &Vec<Range> {
        &self.from_ranges
    }
}

fn format_call_hierarchy_calls<'a>(
    meta: EditorMeta,
    ctx: &mut Context,
    incoming_or_outgoing: bool,
    item: &'a CallHierarchyItem,
    result: &'a (LanguageId, Option<Vec<impl CallHierarchyCall<'a>>>),
) {
    let (language_id, result) = result;
    let ServerSettings { root_path, .. } = &ctx.language_servers[language_id];
    let result = match result {
        Some(result) => result,
        None => return,
    };

    let first_line_suffix = format!(
        "{} - list of {}",
        &item.name,
        if incoming_or_outgoing {
            "callers"
        } else {
            "callees"
        },
    );

    let contents = format_location(
        &meta,
        ctx,
        root_path,
        &item.uri,
        item.range.start,
        "",
        &first_line_suffix,
    ) + &result
        .iter()
        .map(|call| {
            let caller = call.caller(item);
            let callsite_filename = caller.uri.to_file_path().unwrap();
            let caller_or_calle = call.caller_or_callee();

            format_location(
                &meta,
                ctx,
                root_path,
                &caller_or_calle.uri,
                caller_or_calle.range.start,
                "  ",
                &caller_or_calle.name,
            ) + &call
                .callsites()
                .iter()
                .map(|range| {
                    let line = get_file_contents(callsite_filename.to_str().unwrap(), ctx)
                        .map(|text| text.line(range.start.line as usize).to_string())
                        .unwrap_or_default();
                    let line = line
                        .strip_suffix("\r\n")
                        .or_else(|| line.strip_suffix('\n'))
                        .unwrap_or(&line);
                    format_location(
                        &meta,
                        ctx,
                        root_path,
                        &caller.uri,
                        range.start,
                        "    ",
                        line,
                    )
                })
                .join("")
        })
        .join("");

    let command = if incoming_or_outgoing {
        "lsp-show-incoming-calls"
    } else {
        "lsp-show-outgoing-calls"
    };
    let command = format!(
        "{} {} {}",
        command,
        editor_quote(&root_path),
        editor_quote(&contents),
    );
    ctx.exec(meta, command);
}
