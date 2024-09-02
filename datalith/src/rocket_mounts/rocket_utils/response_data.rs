use datalith_core::{mime::Mime, ReadableDatalithFile};
use rocket_etag_if_none_match::entity_tag::EntityTag;

#[derive(Debug)]
pub struct ResponseData {
    pub etag:      EntityTag<'static>,
    pub file:      ReadableDatalithFile,
    pub download:  bool,
    pub file_name: String,
    pub file_type: Mime,
}
