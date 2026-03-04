use crate::{
    configuration::{Environment, Settings},
    crawler::Stats,
};
use lettre::{
    Message, SendmailTransport, SmtpTransport, Transport,
    message::{MultiPart, SinglePart, header::ContentType},
};

pub fn send(config: &Settings, files: &[(String, String, Stats, bool)]) {
    let body = get_body_html(
        &config.application.reports_folder,
        &config.application.reports_server,
        files,
    );

    let mut builder = Message::builder()
        .from(config.email.send_from.parse().unwrap())
        .to(config.email.send_to.parse().unwrap());

    for bcc in &config.email.send_bcc {
        builder = builder.bcc(bcc.parse().unwrap());
    }

    let email = builder
        .subject(config.email.subject.parse::<String>().unwrap())
        .multipart(
            MultiPart::alternative().singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(body),
            ),
        )
        .unwrap();

    let environment: Environment = std::env::var("APP_ENVIRONMENT")
        .unwrap_or_else(|_| "local".into())
        .try_into()
        .expect("Failed to parse APP_ENVIRONMENT.");

    match environment {
        Environment::LOCAL => {
            let mailer = SmtpTransport::builder_dangerous("localhost") // or the container name
                .port(1025) // MailCatcher's default SMTP port
                .build();
            mailer.send(&email).unwrap();
        }
        Environment::PRODUCTION => {
            let mailer = SendmailTransport::new();
            mailer.send(&email).unwrap();
        }
    };
}

fn get_body_html(
    reports_folder: &str,
    reports_server: &str,
    files: &[(String, String, Stats, bool)],
) -> String {
    let list_items: String = files
        .iter()
        .map(|l| {
            //---------- A tag is formatted based on if there were any errors
            let url = escape_html(&l.1.replace(reports_folder, reports_server));
            let a_tag = if l.3 {
                format!(r#"<a href="{}">See errors report</a>"#, url)
            } else {
                String::from("(No errors triggered.)")
            };

            format!(
                r#"<li>File: {}: {} Requests in {:.2} minutes. {}"#,
                l.0, l.2.requests, l.2.minutes, a_tag
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"
        <h3>The crawler finished running. See below stats and error reports:</h3>
        <ul>{list_items}</ul>
    "#
    )
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
