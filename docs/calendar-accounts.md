# Setting up Google and Microsoft calendar access

DayPlan reads Google Calendar and Outlook through an OAuth client for each. It ships with clients already set up, so **connecting an account needs nothing from this guide** — open **Calendars** and select **Connect Google** or **Connect Outlook**. This guide is for the two people who need more: whoever maintains DayPlan's own clients, and anyone who would rather point their build at a client of their own.

Checked against Google's and Microsoft's documentation on September 17, 2026. Consoles change often, so expect labels to drift.

## What DayPlan asks for

Both connections are read-only and stay that way. DayPlan never requests a scope that could change a calendar, and every request it makes to the Google Calendar API or Microsoft Graph is a `GET`. The only `POST`s go to the providers' OAuth endpoints: exchanging the sign-in code, refreshing an access token, and revoking a Google grant on disconnect. [Calendar integration](calendar-integration.md) covers what DayPlan does with the access.

| Provider  | Scopes requested                                    | What that allows                                          |
| --------- | --------------------------------------------------- | --------------------------------------------------------- |
| Google    | `https://www.googleapis.com/auth/calendar.readonly` | List your calendars and read events from them             |
| Microsoft | `Calendars.Read`, `offline_access`                  | Same, plus keeping the sign-in alive without asking again |

DayPlan doesn't ask for a profile scope. The account name it shows is the address on your primary calendar, which comes back with the calendar list.

## Using your own OAuth clients

The client IDs are in [`src-tauri/src/calendar/oauth.rs`](../src-tauri/src/calendar/oauth.rs). They appear in every sign-in URL, so they aren't secrets. A build can point at different ones:

| Build setting                  | Default                                | Where to store it on GitHub |
| ------------------------------ | -------------------------------------- | --------------------------- |
| `DAYPLAN_GOOGLE_CLIENT_ID`     | The committed Google client ID         | Repository variable         |
| `DAYPLAN_GOOGLE_CLIENT_SECRET` | Not sent                               | Repository secret           |
| `DAYPLAN_MICROSOFT_CLIENT_ID`  | The committed Microsoft application ID | Repository variable         |

The build workflows already pass all three through, so setting the repository variable or secret is the whole job; a local build takes them from the environment instead. They're read at compile time, the same way `DAYPLAN_UPDATER_PUBKEY` is, so a rebuild is what picks up a change, and an empty value falls back to the committed client rather than shipping a client ID nothing recognizes.

Google lists the client secret as optional for installed apps and DayPlan sends none; set `DAYPLAN_GOOGLE_CLIENT_SECRET` only if your own client's token exchange turns out to want one. Never commit it — Google itself says an installed app can't keep a secret private, which is exactly why DayPlan doesn't depend on one.

Release signing matters here too. macOS ties keychain access to the app's code signature, so without a stable Developer ID signature, every update asks again for access to each saved token. See the [release checklist](release-checklist.md).

## Google Calendar

### 1. Create the project and turn on the API

1. In the [Google Cloud console](https://console.cloud.google.com/), create a project, such as **DayPlan**.
2. Go to **APIs & Services → Library**, find **Google Calendar API**, and select **Enable**.

### 2. Set up the consent screen

OAuth settings live under **Google Auth Platform**, with sections for Overview, Branding, Audience, Data Access, Clients, and Verification Center.

1. **Branding → Get started:** enter the app name (**DayPlan**) and a support email.
2. **Audience:** choose **External**, add a contact email, accept the user data policy, and create.
3. **Audience → Test users:** add the Google accounts you'll test with, up to 100.
4. **Data Access → Add or remove scopes:** add `https://www.googleapis.com/auth/calendar.readonly`, which lists calendars and reads events from every calendar the user can see. It's a _sensitive_ scope, not a _restricted_ one.

   The narrower pair `calendar.calendarlist.readonly` and `calendar.events.readonly` covers the same ground and may be an easier sell in verification, which asks why a narrower scope isn't enough. DayPlan asks for the single scope: two scopes means two tick boxes on the consent screen, and a user who leaves one ticked and the other not gets a connection that half works.

### 3. Create the client

1. **Clients → Create client**, choose **Desktop app**, name it (for example, **DayPlan desktop**), and create it.
2. **Copy the client ID.** Google also issues a secret; it's shown only once, so copy it too if you want it on hand, though DayPlan doesn't send one.
3. There's no redirect URI to register. Desktop clients accept loopback redirects on any port, and DayPlan uses `http://127.0.0.1:<port>` — Google recommends the IP literal over `localhost`, which client firewalls sometimes get in the way of.
4. Save the ID as the `DAYPLAN_GOOGLE_CLIENT_ID` variable in the repository's **Settings → Secrets and variables → Actions**. If you also need the secret, save it as the `DAYPLAN_GOOGLE_CLIENT_SECRET` secret there. Don't commit the secret.

DayPlan uses the authorization code flow with PKCE (S256), so the sign-in is bound to the one request that started it.

### 4. Testing, publishing, and verification

| Publishing status           | Who can connect                                           | Catch                                                               |
| --------------------------- | --------------------------------------------------------- | ------------------------------------------------------------------- |
| Testing                     | Only test users, up to 100                                | Tokens expire **7 days** after consent, so testers reconnect weekly |
| In production, not verified | Anyone, after a "Google hasn't verified this app" warning | A lifetime cap of **100 new users** for the project                 |
| In production, verified     | Anyone                                                    | Verification takes work and re-review when scopes change            |

For your own use, Testing (or unverified production for a handful of people) is enough. To verify for a public release:

1. Complete **Branding**: logo, a public homepage, a privacy policy on the same domain, and authorized domains. Verify domain ownership in Google Search Console as a project Owner or Editor.
2. Select **Verify branding**. The automated check takes minutes; a manual review takes 2–3 business days.
3. In **Verification Center**, justify each scope and link an unlisted YouTube video that shows the English consent screen, the client ID in the browser's address bar, and DayPlan using the calendar data.
4. Sensitive-scope review typically takes 3–5 business days. Calendar scopes don't need a CASA security assessment, which applies only to restricted scopes.

Keep in mind:

- Clients unused for 6 months are deleted automatically, with an email 30 days before.
- Asking for one scope sidesteps Google's granular permissions: with several, a user can untick some and leave the app with a partial grant.

## Microsoft Outlook

### 1. Get a directory to register the app in

A personal Microsoft account alone can no longer register apps; registration needs a Microsoft Entra tenant.

- **Simplest route:** create a free [Azure account](https://azure.microsoft.com/pricing/purchase-options/azure-account) with your Microsoft account. It asks for a card for identity verification (a temporary $1 hold) and creates a Microsoft Entra ID Free tenant.
- **Microsoft 365 Developer Program sandbox:** now only for Visual Studio Professional or Enterprise subscribers and qualifying Microsoft partners.

### 2. Register the app

1. In the [Microsoft Entra admin center](https://entra.microsoft.com/), go to **Entra ID → App registrations → New registration**.
2. Name it **DayPlan**. Under supported account types, choose **Accounts in any organizational directory (any Microsoft Entra ID tenant) and personal Microsoft accounts**. Leave the redirect URI empty and register.
3. Go to **Authentication → Add a platform → Mobile and desktop applications**, add the custom redirect URI `http://localhost`, and save. Microsoft ignores the port when matching a `localhost` reply URL, so DayPlan's random loopback port works — and that port-ignoring is the reason Outlook sign-in uses `localhost` where Google uses `127.0.0.1`. The portal's redirect box won't accept an `http://127.0.0.1` URL at all; adding one means editing the app manifest, and the port would then have to match exactly.
4. Don't create a client secret. DayPlan is a public client and uses PKCE; Microsoft rejects secrets from public clients.
5. Go to **API permissions → Add a permission → Microsoft Graph → Delegated permissions** and add exactly two:
   - `Calendars.Read` to list calendars and read events from them.
   - `offline_access` so DayPlan can refresh access without asking again.

   Not `User.Read`, `openid`, `profile`, or `email`: DayPlan names the connected account from the owner of its default calendar, so it never needs to read a profile. `Calendars.ReadBasic` is narrower still — it withholds event bodies, attachments, and extensions, none of which DayPlan asks Graph for — so it should be enough for a registration of your own, though DayPlan's own client uses `Calendars.Read` and that's the combination that's been tested.

6. From **Overview**, copy the **Application (client) ID** and save it as the `DAYPLAN_MICROSOFT_CLIENT_ID` repository variable.

### 3. Who can connect

- **Personal accounts** (outlook.com, hotmail.com, live.com) can approve DayPlan themselves.
- **Work and school accounts** usually can't. Most tenants' default consent policy blocks users from approving calendar permissions, and an unverified multitenant app triggers an admin-approval request. Someone with an admin role in that organization has to approve DayPlan, or the user sends them a request.
- **Publisher verification**, which removes the "unverified" label, needs a Microsoft AI Cloud Partner Program account for a legal entity, an app registered in a work tenant (not with a personal account), and a DNS-verified domain. It isn't available to individuals.

DayPlan signs in through `https://login.microsoftonline.com/common/oauth2/v2.0/authorize` and reads events from Microsoft Graph's `calendarView`, which returns recurring events already expanded. Refresh tokens last 90 days, renew each time they're used, and are replaced on every refresh — DayPlan stores the new one and drops the old.

## Or subscribe with a link

A link works without any sign-in, and it's the way in for calendars DayPlan can't connect to — a work account whose tenant won't approve the app, or an Apple, Fastmail, or Proton calendar.

In DayPlan, open **Calendars** (⌘6, or Ctrl+6 on Windows), paste a link under **Subscribe by link**, and DayPlan refreshes the calendar every 30 minutes. The link is stored in your system keychain.

- **Google Calendar (web):** Settings → under **Settings for my calendars**, choose the calendar → **Integrate calendar** → copy **Secret address in iCal format**. Reset it there if the link ever leaks. Google Workspace admins can hide this address.
- **Outlook.com and the new Outlook on the web:** Settings → **Calendar** → **Shared calendars** → **Publish a calendar**. Choose the calendar and how much detail to share, select **Publish**, and copy the **ICS** link. **Unpublish** revokes it. Microsoft 365 admins can limit publishing.
- **Anything else:** look for a public or private iCal, `.ics`, or `webcal://` link, or export an `.ics` file and use **Import .ics file**.

Anyone with a private link can read that calendar, so treat it like a password.

## Sources

- Google: [OAuth for installed apps](https://developers.google.com/identity/protocols/oauth2/native-app), [configure the consent screen](https://developers.google.com/workspace/guides/configure-oauth-consent), [manage OAuth clients](https://support.google.com/cloud/answer/15549257), [audience and publishing status](https://support.google.com/cloud/answer/15549945), [unverified apps](https://support.google.com/cloud/answer/7454865), [sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification), [Calendar API scopes](https://developers.google.com/workspace/calendar/api/auth), [granular permissions](https://developers.google.com/identity/protocols/oauth2/resources/granular-permissions), [OAuth policies](https://developers.google.com/identity/protocols/oauth2/policies), [secret iCal address](https://support.google.com/calendar/answer/37648)
- Microsoft: [register an app](https://learn.microsoft.com/entra/identity-platform/quickstart-register-app), [desktop app registration](https://learn.microsoft.com/entra/identity-platform/scenario-desktop-app-registration), [redirect URI rules](https://learn.microsoft.com/entra/identity-platform/reply-url), [authorization code flow](https://learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow), [Graph permissions](https://learn.microsoft.com/graph/permissions-reference), [consent policies](https://learn.microsoft.com/entra/identity/enterprise-apps/manage-app-consent-policies), [publisher verification](https://learn.microsoft.com/entra/identity-platform/publisher-verification-overview), [create a tenant](https://learn.microsoft.com/entra/identity-platform/quickstart-create-new-tenant), [calendarView](https://learn.microsoft.com/graph/api/user-list-calendarview), [refresh tokens](https://learn.microsoft.com/entra/identity-platform/refresh-tokens), [publish an Outlook.com calendar](https://support.microsoft.com/office/share-your-calendar-in-outlook-com-0fc1cb48-569d-4d1e-ac20-5a9b3f5e6ff2)
