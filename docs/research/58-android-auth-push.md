# Android authentication and push: primary-source notes for #58

Verified 2026-10-07. These are implementation recommendations, not a record of
live Google, GitHub or FCM validation.

## Authentication

Credential Manager supports passkeys, passwords and Google ID tokens. Its
documented provider integration does not include GitHub. GitHub documents
browser authorization-code and device-authorization flows. Therefore use
Credential Manager for Google and passkeys, and the existing official GitHub
authorization-code flow in a Custom Tab. This is a platform compatibility
decision; do not invent a GitHub credential type or move provider tokens into
the Android app. Preserve the official service's explicit `user:email` scope,
PKCE, verified-email linking rules and identity lookup. [Credential Manager
FAQ](https://developer.android.com/identity/sign-in/credential-manager-faq),
[GitHub OAuth](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps).

Google Credential Manager returns a `GoogleIdTokenCredential`. The relying party
server must validate it before trusting the identity. Configure its server
client ID and a one-use nonce; reuse the existing official account-linking
decision after validating the token's signature, issuer, audience and nonce.
Native ID-token verification needs a native exchange form of the existing
official Google authentication contract; the browser callback expects an
authorization code and cannot consume an ID token unchanged. [Android Google
integration](https://developer.android.com/identity/sign-in/credential-manager-siwg-implementation).

Native passkeys can use the existing registration, login and reauthentication
start/finish routes, JSON challenges, credential store and RP ID. The native
credential origin is `android:apk-key-hash:` followed by the base64url SHA-256
signing certificate fingerprint. The service must allow only configured trusted
signing certificates; never accept a caller-provided arbitrary origin.
`WebauthnBuilder::append_allowed_origin` explicitly supports native origins.
The official HTTPS RP host must publish `/.well-known/assetlinks.json` containing
`dev.leo.manager`, trusted SHA-256 certificate fingerprints and
`delegate_permission/common.get_login_creds`. Native passkeys require Android 9
(API 28) or later, despite the app's API 26 minimum; retain alternative sign-in
methods below 28. [Native passkey creation](https://developer.android.com/identity/passkeys/create-passkeys),
[Digital Asset Links](https://developer.android.com/identity/credential-manager/prerequisites),
[webauthn-rs builder](https://docs.rs/webauthn-rs/0.5.3/webauthn_rs/struct.WebauthnBuilder.html#method.append_allowed_origin).

## Native push

FCM HTTP v1 is the simplest Google-APIs-compatible transport. Send data-only
messages: notification payloads are displayed automatically by Android when the
app is in the background and bypass application authorization checks. Data
messages reach `FirebaseMessagingService.onMessageReceived` in both foreground
and background. Keep the receiver unexported. Schedule longer work with
WorkManager instead of assuming an unlimited callback lifetime. [FCM Android
reception](https://firebase.google.com/docs/cloud-messaging/android/receive-messages).

Recommendation inferred from the ticket's immediate-revocation contract:
reuse #56's current-account/current-membership recipient checks and delivery
serialization; register one FCM token per device/account. Carry only generic
event kind and identifiers. Before displaying a delayed data message, ask the
official authenticated interface whether that installation is still accessible;
fail closed if the session or membership is gone. This closes the queued-message
case after removal without an installation-specific subscription.

The sender uses
`POST https://fcm.googleapis.com/v1/projects/{project_id}/messages:send`, with a
short-lived OAuth bearer token and the scope
`https://www.googleapis.com/auth/firebase.messaging`. For a service account,
sign an RS256 JWT with `iss=client_email`, that scope,
`aud=https://oauth2.googleapis.com/token`, `iat` and `exp` (at most one hour).
Exchange the assertion at that token endpoint using form grant type
`urn:ietf:params:oauth:grant-type:jwt-bearer`. Cache access tokens before expiry;
keep the service-account private key entirely in the official service. Existing
Rust HTTP and cryptography libraries can implement this adapter without an
Android Firebase Auth dependency. [FCM HTTP v1 authorization](https://firebase.google.com/docs/cloud-messaging/send/v1-api),
[Google service-account OAuth](https://developers.google.com/identity/protocols/oauth2/service-account).

## Verified stable dependency pins

Use existing project runtime/tool pins. New Android library versions verified in
the owning release notes:

| Dependency | Version | Primary source |
| --- | --- | --- |
| `androidx.credentials:credentials` | `1.6.0` | [AndroidX release notes](https://developer.android.com/jetpack/androidx/releases/credentials) |
| `androidx.credentials:credentials-play-services-auth` | `1.6.0` | [AndroidX release notes](https://developer.android.com/jetpack/androidx/releases/credentials) |
| `com.google.android.libraries.identity.googleid:googleid` | `1.2.1` | [Google ID release notes](https://developers.google.com/identity/android-credential-manager/releases) |
| `com.google.firebase:firebase-messaging` | `25.1.3` | [Firebase Android release notes](https://firebase.google.com/support/release-notes/android) |

If Firebase initialization is supplied directly through `FirebaseOptions`, the
Google services Gradle plugin and a checked-in `google-services.json` are not
needed (an inference from the explicit options initialization interface).
[FirebaseApp initialization](https://firebase.google.com/docs/reference/android/com/google/firebase/FirebaseApp).
Keep configuration absent-by-default for local/CI builds, and report
real-provider tests separately from tested fake adapters and emulator execution.
