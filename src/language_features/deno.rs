use std::{collections::HashMap, path::PathBuf};

use itertools::Itertools;
use lsp_types::{request::Request, Location, TextDocumentIdentifier, Uri};
use ropey::Rope;

use crate::{
    context::{Context, RequestParams},
    position::lsp_range_to_kakoune,
    types::{BackwardKakouneRange, EditorMeta, ServerId},
    util::{self, editor_quote, short_file_path},
};

struct VirtualTextDocument {}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct VirtualTextDocumentParams {
    text_document: TextDocumentIdentifier,
}

impl Request for VirtualTextDocument {
    type Params = VirtualTextDocumentParams;
    type Result = String;

    const METHOD: &'static str = "deno/virtualTextDocument";
}

pub fn handle_virtual_locations(
    meta: &EditorMeta,
    ctx: &mut Context,
    locations: Vec<(ServerId, Location)>,
) {
    if locations.is_empty() {
        return;
    }

    let unique_uris: Vec<(ServerId, Uri)> = locations
        .iter()
        .map(|(server_id, Location { uri, .. })| (*server_id, uri.clone()))
        .unique()
        .collect();

    let req_params: HashMap<usize, Vec<VirtualTextDocumentParams>> =
        unique_uris
            .iter()
            .fold(HashMap::new(), |mut m, (server_id, uri)| {
                m.entry(*server_id)
                    .or_default()
                    .push(VirtualTextDocumentParams {
                        text_document: TextDocumentIdentifier {
                            uri: (*uri).clone(),
                        },
                    });
                m
            });

    ctx.call::<VirtualTextDocument, _>(
        meta.clone(),
        RequestParams::Each(req_params),
        move |ctx, meta, results| {
            let content_map: HashMap<Uri, String> = unique_uris
                .iter()
                .zip(results.into_iter())
                .map(|((_, uri), (_, content))| (uri.clone(), content))
                .collect();

            // Write temp files for each unique URI.
            let tmp_paths: HashMap<&str, PathBuf> = unique_uris
                .iter()
                .map(|(_, uri)| {
                    let content = content_map.get(uri).unwrap();
                    let tmp_path = util::create_virtual_definition_file("deno", uri, content);
                    (uri.as_str(), tmp_path)
                })
                .collect();

            if locations.len() == 1 {
                let (server_id, Location { uri, range }) = &locations[0];
                let content = content_map.get(uri).unwrap();
                let tmp_path = tmp_paths.get(uri.as_str()).unwrap();

                // Select range that the server wants to show.
                let text = Rope::from_str(content);
                let server = ctx.server(*server_id);
                let kak_range = lsp_range_to_kakoune(range, &text, server.offset_encoding);
                let command = format!(
                    "evaluate-commands -try-client %opt{{jumpclient}} -- %[
                        edit -existing {}; \
                        select {}; \
                        execute-keys <c-s>vv \
                    ]",
                    editor_quote(&tmp_path.to_string_lossy()),
                    BackwardKakouneRange(kak_range)
                );
                ctx.exec(meta, command);

                return;
            }

            let select_location = locations
                .iter()
                .map(|(server_id, Location { uri, range })| {
                    let content = content_map.get(uri).unwrap();
                    let rope = Rope::from_str(content);
                    let server = ctx.server(*server_id);
                    let pos = lsp_range_to_kakoune(range, &rope, server.offset_encoding).start;
                    if range.start.line as usize >= rope.len_lines() {
                        return "".into();
                    }
                    let tmp_path = tmp_paths.get(uri.as_str()).unwrap();
                    format!(
                        "{}:{}:{}:{}",
                        short_file_path(&tmp_path.to_string_lossy(), ctx.main_root(&meta)),
                        pos.line,
                        pos.column,
                        rope.line(range.start.line as usize),
                    )
                })
                .join("");

            let command = format!(
                "lsp-show-goto-choices {} {}",
                editor_quote(ctx.main_root(&meta)),
                editor_quote(&select_location),
            );
            ctx.exec(meta, command);
        },
    );
}
