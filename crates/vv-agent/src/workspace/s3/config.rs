#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3WorkspaceConfig {
    pub bucket: String,
    pub prefix: String,
    pub endpoint_url: Option<String>,
    pub region_name: Option<String>,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_session_token: Option<String>,
    pub addressing_style: String,
}

impl S3WorkspaceConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: String::new(),
            endpoint_url: None,
            region_name: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            addressing_style: "virtual".to_string(),
        }
    }
}
