// Verifies the README basic example compiles against the real API.
use rs_retryablehttp::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().build();

    let req = reqwest::Request::new(
        reqwest::Method::GET,
        "https://example.com/api/data".parse()?,
    );
    let _resp = client.execute(req).await?;
    Ok(())
}
