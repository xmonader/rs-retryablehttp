// Verifies the README "Handling errors" example compiles and exhaustively
// matches every `Error` variant against the real API.
use rs_retryablehttp::{Client, Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().build();
    let req = reqwest::Request::new(
        reqwest::Method::GET,
        "https://example.com/api/data".parse()?,
    );

    match client.execute(req).await {
        Ok(resp) => println!("got {}", resp.status()),
        Err(Error::MaxRetriesExceeded { attempts, source }) => match source {
            Some(cause) => eprintln!("gave up after {attempts} attempts: {cause}"),
            None => eprintln!("gave up after {attempts} attempts (server kept failing)"),
        },
        Err(Error::BodyNotRewindable) => eprintln!("streaming body can't be retried"),
        Err(Error::Reqwest(e)) => eprintln!("request failed without retry: {e}"),
    }
    Ok(())
}
