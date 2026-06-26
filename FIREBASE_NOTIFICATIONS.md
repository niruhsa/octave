# Real-time push notifications (Firebase Cloud Messaging)

This guide sets up **real-time new-release notifications on Android** via Firebase
Cloud Messaging (FCM). With it configured, when someone you follow gets a new
release, the notification arrives **instantly — even when the app is closed /
swiped away** (the OS displays it without the app running).

It is **entirely optional**. If you don't set this up, the project still builds
and runs:

- **Server** runs normally; it just doesn't push (it only persists
  notifications for clients to fetch).
- **Android app** falls back to a ~15-minute background poll (WorkManager) and a
  foreground in-app poller.
- **Desktop app** uses the foreground poller + OS notifications (FCM is
  Android-only; desktop never uses it).

> **iOS is not supported** — this project isn't released for iOS, so APNs is
> intentionally not implemented.

---

## How it works (1-minute overview)

1. The Android app fetches its **FCM registration token** and registers it with
   the server (this happens automatically right after you sign in).
2. When a followed artist gets a new release, the server's fan-out POSTs a
   message to the **FCM HTTP v1 API** for each follower's registered devices.
3. Firebase delivers it; Android **auto-displays** the notification in the
   system tray — no app process required.

The server authenticates to FCM with a Google **service-account key**. The
Android app identifies its Firebase project with **`google-services.json`**.
These are **two different files** — see the table below.

---

## The two files you'll download

You will download **two separate JSON files** from the Firebase console. They
are not interchangeable.

| | `google-services.json` | service-account key |
|---|---|---|
| **Used by** | the Android app (client) | the server |
| **Goes in** | `app/src-tauri/gen/android/app/` | the server host (e.g. `server/secrets/`) |
| **Downloaded from** | Project Settings → **General** → your Android app | Project Settings → **Service accounts** → *Generate new private key* |
| **Contains** | project id, app id, a *client* API key | `client_email` + an RSA **private key** |
| **Secret?** | No (ships inside the APK) | **Yes — never commit or ship it** |

Both must come from the **same Firebase project**.

---

## Prerequisites

- A Google account (Firebase is free for this).
- To build the Android app (see [`app/AGENTS.md`](./app/AGENTS.md)):
  - **JDK 17** (AGP/Gradle here don't support newer), `ANDROID_HOME` pointing at
    an SDK, and an installed **NDK**.
  - Node + `npm install` in `app/`.
- The test device must have **Google Play Services** (standard on most Android
  phones/emulators with the Play Store; absent on some custom ROMs — there FCM
  silently falls back to the WorkManager poll).

---

## Step 1 — Create a Firebase project

1. Go to <https://console.firebase.google.com/> → **Add project**.
2. Name it anything (e.g. `octave`). Analytics is optional — you can skip it.
3. Note your **Project ID** (Project Settings → General → *Project ID*, e.g.
   `octave-1a2b3`). You'll need it for the server config.

---

## Step 2 — Register the Android app + place `google-services.json`

1. In the Firebase console: **Project Settings** (gear icon) → **General** →
   **Your apps** → **Add app** → **Android**.
2. **Android package name** — this must match the app's id **exactly**:
   ```
   dev.niruhsa.octave
   ```
   (Defined in [`app/src-tauri/gen/android/app/build.gradle.kts`](./app/src-tauri/gen/android/app/build.gradle.kts)
   as `applicationId` / `namespace`.) A mismatch makes the build fail with
   "No matching client found for package name".
3. The SHA-1 / app nickname fields are optional — skip them.
4. Download **`google-services.json`** and drop it here:
   ```
   app/src-tauri/gen/android/app/google-services.json
   ```
   That's the Android *app module* directory (next to `build.gradle.kts`). The
   build applies the Firebase Gradle plugin **only if this file exists**, so
   placing it is what turns FCM on for the client.

> ℹ️ `app/src-tauri/gen/android/` is **generated** by Tauri. If you ever
> regenerate the Android project (`tauri android init`), re-place this file and
> re-apply the native customizations (same caveat as the existing media/upload
> services).

---

## Step 3 — Generate the service-account key + place it on the server

1. In the Firebase console: **Project Settings** → **Service accounts** →
   **Generate new private key** → confirm. A JSON file downloads.
2. Put it on the **server** machine, somewhere outside version control, e.g.:
   ```
   server/secrets/fcm-service-account.json
   ```
   ```sh
   mkdir -p server/secrets
   mv ~/Downloads/<project>-firebase-adminsdk-*.json server/secrets/fcm-service-account.json
   ```
3. **Keep it secret.** Add it to `.gitignore` so it can never be committed:
   ```sh
   echo "server/secrets/" >> .gitignore
   ```

> This key authorizes sending pushes to your project. Treat it like a password.

---

## Step 4 — Configure the server

The server reads config from `server/.env` (copy from
[`server/.env.example`](./server/.env.example) if you haven't). Add:

```env
FCM_ENABLED=1
FCM_PROJECT_ID=octave-1a2b3
FCM_CREDENTIALS=secrets/fcm-service-account.json
```

- `FCM_PROJECT_ID` — your **Project ID** from Step 1 (must match the project the
  service-account key belongs to).
- `FCM_CREDENTIALS` — path to the key from Step 3. **Relative paths resolve
  against the `.env` directory** (i.e. `server/`), so `secrets/...` →
  `server/secrets/...`. Absolute paths work too.

When `FCM_ENABLED=1`, both other vars are **required** — the server fails fast at
startup if either is missing or the key file is unreadable/malformed (so a
broken push setup never boots silently).

---

## Step 5 — Build & run

**Server:**

```sh
cd server
cargo run   # or: cargo build --release && ./target/release/server
```

On startup you should see a log line confirming push is on:

```
INFO ... FCM push enabled project=octave-1a2b3
```

(If you see a config error instead, re-check the two `FCM_*` paths/values.)

**Android app** (a full native rebuild is required — a JS reload won't pick up
the Gradle/Firebase changes):

```sh
cd app
export JAVA_HOME="$(/usr/libexec/java_home -v 17)"   # macOS; adjust per OS
npm install
npm run tauri android dev        # or: npm run tauri android build
```

Install/run it on a device or emulator **with Google Play Services**.

---

## Step 6 — Verify end-to-end

1. **Sign in** to the app as a normal user (username/password — *not* a
   `SECRET_KEY` session; only real users have per-user notifications). Right
   after sign-in the app registers its FCM token with the server automatically.
2. **Follow an artist** (the ♥ Follow button on an Artist page).
3. **Swipe the app away** (remove it from recents) so you're testing the
   while-closed path.
4. As a manager, **add a new release** for that artist — upload a track/album, or
   create the album via the API — so the server's new-release fan-out runs.
5. Within a few seconds, the **notification appears in the system tray**, even
   with the app closed. Tapping it opens the app.

### Watching the logs while testing

On the device (via `adb`):

```sh
adb logcat -s OctavePush:* OctaveNotif:*
```

- `OctavePush` — token fetch/registration + the FCM service.
- `OctaveNotif` — the WorkManager poll fallback (you'll see this instead if FCM
  is unavailable).

On the server, watch for the `FCM push enabled` startup line and any
`fcm send ...` warnings.

---

## Behavior without Firebase (the gating)

Everything is gated so the project is fully functional before you do any of this:

- **Server**: with `FCM_ENABLED` unset/`0`, no push is attempted; clients poll.
- **Android**: without `google-services.json`, the Firebase Gradle plugin isn't
  applied and FCM init returns no token → the app **falls back to the ~15-minute
  WorkManager background poll** (and the instant foreground poller). When FCM
  *is* available, the app disables that poll (FCM covers the while-closed case),
  avoiding duplicate notifications.
- **Desktop**: never uses FCM; uses the foreground poller + OS notifications.

So you can develop/build without Firebase and add it later with no code changes.

---

## Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| Gradle: *"No matching client found for package name 'dev.niruhsa.octave'"* | The Android app registered in Firebase (Step 2) has a different package name. Re-add the app with exactly `dev.niruhsa.octave`, re-download `google-services.json`. |
| Server won't start, `configuration error: FCM_ENABLED is on but ...` | `FCM_PROJECT_ID` or `FCM_CREDENTIALS` missing/empty, or the key path is wrong (remember it's relative to `server/`). |
| Server logs `fcm token http 401/403` or `fcm send http 403` | The service-account key is from a different project than `FCM_PROJECT_ID`, or the **Firebase Cloud Messaging API (V1)** is disabled. Enable it: Google Cloud Console → APIs & Services → *Firebase Cloud Messaging API* → Enable. Regenerate the key from the **Firebase** console (it gets the right role). |
| Server logs `fcm jwt sign` / token errors | Malformed service-account JSON, or large clock skew on the server (the signed JWT is time-bound — keep the server clock synced via NTP). |
| No notification, `OctaveNotif` logs appear (not `OctavePush`) | FCM is unavailable on the device (no Play Services, or `google-services.json` missing from the build) → it fell back to the 15-min poll. Test on a device with Play Services and rebuild with the JSON in place. |
| Notification never shows even via the poll | Notifications permission denied — grant `POST_NOTIFICATIONS` (Android 13+) in the app's system settings. |
| Still nothing after a real release | Confirm you followed the artist **and** that the *follower's* device is signed in + registered (check `OctavePush` logs right after sign-in), and that the manager who added the release is a *different* user (the actor is excluded from their own new-release fan-out). |

---

## Security & notes

- **Never commit the service-account key** (or ship it in the app). Only the
  server needs it. `google-services.json` is not secret (it's inside the APK),
  but it's project-specific — commit it or not, your choice.
- All three of `FCM_PROJECT_ID`, the service-account key, and
  `google-services.json` must belong to the **same Firebase project**.
- The bearer/session token used to register a device is read **in Rust from the
  secure store** and never passes through the WebView.
- On **sign-out**, the app unregisters its token first, so a signed-out device
  stops receiving that user's notifications.
- FCM requires **Google Play Services**; on devices without it, the app
  automatically uses the WorkManager poll instead.

For the implementation details, see
[`server/AGENTS.md`](./server/AGENTS.md) and [`app/AGENTS.md`](./app/AGENTS.md).
