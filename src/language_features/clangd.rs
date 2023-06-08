use std::collections::HashMap;

use crate::context::*;
use crate::types::*;
use crate::util::*;
use lsp_types::request::Request;
use lsp_types::*;

pub struct SwitchSourceHeaderRequest {}

impl Request for SwitchSourceHeaderRequest {
    type Params = TextDocumentIdentifier;
    type Result = Option<Url>;
    const METHOD: &'static str = "textDocument/switchSourceHeader";
}

pub fn switch_source_header(meta: EditorMeta, ctx: &mut Context) {
    let (language_id, srv_settings) = meta
        .language
        .and_then(|id| ctx.language_servers.get_key_value(&id))
        .or_else(|| ctx.language_servers.first_key_value())
        .unwrap();
    let mut req_params = HashMap::new();
    req_params.insert(
        language_id.clone(),
        vec![TextDocumentIdentifier {
            uri: Url::from_file_path(&meta.buffile).unwrap(),
        }],
    );
    ctx.call::<SwitchSourceHeaderRequest, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx, meta, results| {
           if let Some((_,response)) = results.into_iter().find(|(_,v)| v.is_some()) {
                if let Some(response) = response {
                    let command = format!(
                        "evaluate-commands -try-client %opt{{jumpclient}} -verbatim -- edit -existing {}",
                        editor_quote(response.to_file_path().unwrap().to_str().unwrap()),
                    );
                    ctx.exec(meta, command);
                }
            }
        },
    );
}
