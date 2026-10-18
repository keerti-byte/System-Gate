# System Gate: setup

There are two roles. **Players** (whoever uses the app) only click **Sign in with Google**. **The developer**
(whoever builds the app) does the Google Cloud step below **once**. Players never see the Cloud Console.

## For players

1. Install and open System Gate.
2. Click **Sign in with Google**. A browser tab opens; approve **Tasks** and **Calendar**.
3. Done. Your quests appear, and the Gate seals itself while you have unfinished ones.

Ticking a quest in System Gate also ticks it in Google Tasks. Calendar events are only used for the schedule
(they are never ticked).

## For the developer: one-time Google Cloud setup (about 10 minutes)

The app ships with *your* Google OAuth client built in. That is the normal way desktop apps work. A desktop
client "secret" is not really secret (Google's docs say so too); the protection is PKCE plus a loopback
redirect, which this app uses.

1. Open <https://console.cloud.google.com/> and create a project (e.g. "System Gate").
2. **APIs & Services → Library**: enable **Google Tasks API** and **Google Calendar API**.
3. **APIs & Services → OAuth consent screen** (Google Auth Platform → Branding / Audience / Data access):
   * App name and your support e-mail.
   * **Data access → Add scopes**: `…/auth/tasks` (view *and edit* tasks), `…/auth/calendar.readonly`,
     plus `openid` and `email`.
   * **Audience**: see "Testing vs. production" below.
4. **APIs & Services → Credentials → Create credentials → OAuth client ID → Application type: Desktop app.**
   Copy the **Client ID** and **Client secret**.
5. Give them to the build:

   ```sh
   cd src-tauri/.cargo
   cp config.toml.example config.toml      # config.toml is git-ignored
   # edit config.toml and paste the two values
   ```

   or set the environment variables `SYSTEM_GATE_GOOGLE_CLIENT_ID` and `SYSTEM_GATE_GOOGLE_CLIENT_SECRET`
   when you run the build (useful in CI).
6. Build: `npm install` then `npm run build` (or `npm run dev` to try it). If the credentials are missing the
   build prints a `cargo:warning=` and the app's sign-in explains what is missing.

Changing the credentials requires a rebuild. They are compiled into the binary.

### Testing vs. production (important)

| Consent screen status | Who can sign in | Gotchas |
|---|---|---|
| **Testing** | Only the Google accounts you list as *test users* (max 100) | Google **expires refresh tokens after 7 days**, so every player must click Sign in again weekly. The app detects this and asks them to. |
| **In production**, unverified | Anyone with the app, up to 100 users | They see an "unverified app" warning once (Advanced → Go to System Gate). No weekly expiry. |
| **In production**, verified | Anyone | Needs Google's verification review because Tasks and Calendar are *sensitive* scopes. Only worth it for a public release. |

For yourself and a few friends: **In production, unverified** is the practical choice.

### Advanced: players with their own credentials

A player can paste their own client ID/secret under *System settings → Google account → Advanced*. That
override wins over the built-in credentials. Nobody needs this normally.

## Security notes

* The credentials in the binary identify *the app*, not a user. Anyone can extract them from the binary; that
  is expected for desktop apps and why PKCE is used. Do not reuse this client for anything with server-side
  privileges.
* Users' tokens are stored locally in the app's config folder (permissions 0600 on Linux/macOS), never sent
  anywhere except Google.
* Do not commit `src-tauri/.cargo/config.toml` (it is in `.gitignore`).
