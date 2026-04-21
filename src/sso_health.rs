use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::App;
use crate::poller;

const SSO_CHECK_INTERVAL: Duration = Duration::from_secs(300);

pub async fn run_sso_health_loop(app: Arc<Mutex<App>>, profile: String, region: String) {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .profile_name(&profile)
        .region(aws_config::Region::new(region))
        .identity_cache(
            aws_config::identity::IdentityCache::lazy()
                .load_timeout(Duration::from_secs(15))
                .build(),
        )
        .load()
        .await;
    let sts_client = poller::aws::StsAuthClient::new(aws_sdk_sts::Client::new(&aws_config));

    loop {
        poller::check_aws_auth(&app, &sts_client, &profile).await;
        tokio::time::sleep(SSO_CHECK_INTERVAL).await;
    }
}
