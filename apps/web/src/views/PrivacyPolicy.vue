<script setup lang="ts">
import BrandWordmark from '../components/BrandWordmark.vue'
import ThemeControl from '../components/ThemeControl.vue'

// Public page: it renders from the document alone and never calls the account API.
// Each statement mirrors Beacon's schema, cleanup and providers; update it with them.
const records = [
  {
    name: 'Your Cairn account',
    what: 'Your verified email address and an account identifier.',
    why: 'To identify your account, send sign-in codes and receive invitations.',
    kept: 'Until you delete your account.',
  },
  {
    name: 'Sign-in methods',
    what: 'For Google or GitHub: the account identifier and email address they return. For passkeys: the public key, its identifier and the name you give it. Cairn has no passwords and keeps no Google or GitHub access token.',
    why: 'To recognise you when you sign in.',
    kept: 'Until you delete your account. A removed method keeps its identifier and label (the email address for email, Google and GitHub methods), without any credential.',
  },
  {
    name: 'Sign-ins in progress',
    what: 'Your email address with a hashed one-time code, or the temporary state of a Google, GitHub or passkey sign-in.',
    why: 'To complete the sign-in you started.',
    kept: '10 minutes for email codes, 5 minutes for other sign-ins. Expired entries are deleted every hour.',
  },
  {
    name: 'Sessions',
    what: 'A hashed session token, when it started, when you last confirmed your identity, and the device description your browser sends (its user agent). Beacon does not keep your IP address with a session.',
    why: 'To keep you signed in and let you review and revoke your sessions.',
    kept: '7 days, or less if you sign out or revoke the session. Expired sessions are deleted every hour.',
  },
  {
    name: 'Installations',
    what: 'For each installation claimed by your account: its name, identifier, hashed credentials, owner, creation date and whether it must be updated. Whether it is online is only kept in memory.',
    why: 'To connect you to the installations you own or have joined.',
    kept: 'Claim codes last 10 minutes. Detaching an installation keeps its record without an owner, so the machine can be claimed again. “Revoke and forget installation” deletes it.',
  },
  {
    name: 'Members and invitations',
    what: 'The email address of each pending invitation, and which accounts are members of an installation and since when. Owners see the email addresses of members and invitees; invitees see the installation name and the owner’s email address.',
    why: 'To share an installation with the people its owner invites.',
    kept: 'Invitations expire after 7 days and are then deleted. Memberships last until the member leaves or is removed, the installation is detached or forgotten, or the account is deleted.',
  },
  {
    name: 'Notification devices',
    what: 'For Web Push, the push address and encryption keys supplied by your browser. For the Android app, its Firebase Cloud Messaging token.',
    why: 'To notify you when an agent asks a question or an installation sends an alert.',
    kept: 'Until you turn notifications off on that device, the push service reports that it is no longer valid, or you delete your account.',
  },
  {
    name: 'External app authorizations (MCP)',
    what: 'When an owner allows an external app to use an installation, or creates an access token for it: the app’s name and redirect addresses, the authorization label, its rights and its expiry. Tokens are stored hashed.',
    why: 'To let that app act on the installation within the rights granted.',
    kept: 'App authorizations last 30 days from their last renewal; access tokens created by an owner last 30 days. Expired ones are deleted every hour, and unused app registrations after 30 days.',
  },
  {
    name: 'Audit log',
    what: 'Changes to account and installation access: the action, the identifiers of the accounts and installation involved, and the date. It holds no email addresses, names, credentials or request content.',
    why: 'To let you review access changes to your account and installations.',
    kept: '90 days, then deleted.',
  },
  {
    name: 'Abuse protection',
    what: 'Request counters keyed by an IP address (the full IPv4 address, or the /64 prefix of an IPv6 address), an account identifier or a hash of an email address.',
    why: 'To limit repeated sign-in attempts, emails and other abuse.',
    kept: 'Deleted one day after the counter’s window ends. Windows last from one minute to one day.',
  },
]

const providers = [
  {
    name: 'Resend',
    use: 'Sends sign-in code and invitation emails. It receives the recipient’s address and the email content: the code, or the installation name and a link to Cairn.',
  },
  {
    name: 'Google',
    use: 'Sign in with Google, only if you choose it. Cairn requests the openid and email scopes. It uses only your Google account identifier, your email address, whether Google has verified it and, for Google Workspace accounts, the domain, used only to check the address; nothing else from your Google profile is kept. Google also delivers Android notifications through Firebase Cloud Messaging; these messages contain only identifiers, no text.',
  },
  {
    name: 'GitHub',
    use: 'Sign in with GitHub, only if you choose it. Cairn requests only the user:email scope, never access to your repositories. It reads your GitHub user identifier and verified primary email address, then revokes the access token immediately.',
  },
  {
    name: 'Your browser’s push service',
    use: 'Delivers Web Push notifications (Google, Mozilla, Apple or Microsoft, depending on your browser). The content is encrypted for your browser: a fixed message when an agent asks a question, or the title and text of an alert sent by your installation.',
  },
  {
    name: 'Cloudflare',
    use: 'Hosts the DNS records of cairn.build. Traffic to Beacon does not pass through Cloudflare.',
  },
]

const cookies = [
  { name: 'cairn_session', use: 'Keeps you signed in.', kept: '7 days' },
  { name: 'cairn_oauth, cairn_passkey, cairn_native_confirmation', use: 'Tie a sign-in in progress to your browser.', kept: '5 minutes' },
]
</script>

<template>
  <main class="min-h-dvh bg-canvas px-6 py-10 text-ink phone:px-4 phone:py-6">
    <div class="mx-auto flex max-w-3xl items-center justify-between gap-4">
      <a href="/" class="rounded-md">
        <BrandWordmark />
      </a>
      <ThemeControl compact />
    </div>
    <article class="mx-auto mt-10 max-w-3xl break-words phone:mt-8">
      <h1>Privacy Policy</h1>
      <p class="mt-2 text-sm text-muted">
        Last updated <time datetime="2026-10-09">October 9, 2026</time>
      </p>

      <section aria-labelledby="privacy-who" class="mt-8 grid gap-3">
        <h2 id="privacy-who">
          Who we are
        </h2>
        <p>
          Cairn lets you run coding agents on your own machines and follow their work from the web and the mobile app.
          Beacon is the Cairn service hosted at cairn.build: it provides Cairn accounts, the interface and access to your installations.
          This policy describes the personal data Beacon handles and why.
        </p>
        <p>
          Beacon is operated by Léo Coletta. For any question or request about your data, write to
          <a class="text-accent underline underline-offset-2" href="mailto:privacy@cairn.build">privacy@cairn.build</a>.
        </p>
      </section>

      <section aria-labelledby="privacy-installation" class="mt-10 grid gap-3">
        <h2 id="privacy-installation">
          What stays on your installation
        </h2>
        <p>
          An installation is the part of Cairn you deploy on your own machines.
          Your conversations, projects, files, coding-agent accounts and secrets are stored there, not on Beacon. When you are a member of an installation shared with you, they are stored on its owner’s machines.
        </p>
        <ul class="grid list-disc gap-2 pl-5">
          <li>
            <strong>Direct connection.</strong> When your network allows it, your device and your installation exchange data directly, without passing through Beacon.
            Beacon only puts them in touch: it passes on a short-lived authorization and the connection parameters, including network addresses, but never conversation content.
          </li>
          <li>
            <strong>Relay.</strong> When a direct connection is not possible, Beacon relays the exchange between your device and your installation.
            Relayed content passes through Beacon’s memory on its way; Beacon does not store it or write it to its logs.
          </li>
        </ul>
      </section>

      <section aria-labelledby="privacy-stored" class="mt-10 grid gap-3">
        <h2 id="privacy-stored">
          What Beacon stores
        </h2>
        <p>Beacon keeps the following in its database. Each item lists what is stored, why, and for how long.</p>
        <dl class="grid gap-4">
          <div v-for="record in records" :key="record.name" class="grid gap-1.5 rounded-xl border border-line bg-surface p-4 phone:p-3">
            <dt class="font-semibold">
              {{ record.name }}
            </dt>
            <dd class="grid gap-1.5 text-sm">
              <p>{{ record.what }}</p>
              <p><span class="text-muted">Purpose: </span>{{ record.why }}</p>
              <p><span class="text-muted">Retention: </span>{{ record.kept }}</p>
            </dd>
          </div>
        </dl>
        <p>
          Beacon does not write IP addresses, email addresses or request content to its application logs.
          Database backups taken before Beacon updates may still contain deleted data until those backups are removed.
        </p>
      </section>

      <section aria-labelledby="privacy-cookies" class="mt-10 grid gap-3">
        <h2 id="privacy-cookies">
          Cookies and browser storage
        </h2>
        <p>Beacon sets only first-party cookies needed to sign you in. They are not readable by scripts.</p>
        <ul class="grid list-disc gap-2 pl-5">
          <li v-for="cookie in cookies" :key="cookie.name">
            <code>{{ cookie.name }}</code>: {{ cookie.use }} Lasts {{ cookie.kept }}.
          </li>
        </ul>
        <p>
          There are no advertising or analytics cookies, and the Cairn interface loads no third-party scripts.
          The interface also keeps preferences, such as your appearance choice and the last installation you opened, and some cached data in your browser’s storage on your device.
        </p>
      </section>

      <section aria-labelledby="privacy-providers" class="mt-10 grid gap-3">
        <h2 id="privacy-providers">
          Service providers
        </h2>
        <p>Beacon uses these providers to run the service:</p>
        <ul class="grid list-disc gap-2 pl-5">
          <li v-for="provider in providers" :key="provider.name">
            <strong>{{ provider.name }}.</strong> {{ provider.use }}
          </li>
        </ul>
        <p>Beacon and its database run on a server in France operated by Léo Coletta. Personal data is not sold or used for advertising.</p>
      </section>

      <section aria-labelledby="privacy-google" class="mt-10 grid gap-3">
        <h2 id="privacy-google">
          Google user data
        </h2>
        <p>
          Cairn’s use and transfer of information received from Google APIs adheres to the
          <a class="text-accent underline underline-offset-2" href="https://developers.google.com/terms/api-services-user-data-policy" rel="noopener noreferrer">Google API Services User Data Policy</a>,
          including the Limited Use requirements.
        </p>
        <p>
          From Google Sign-In, Cairn uses your Google account identifier and email address. It uses them only to sign you in to your Cairn account and to identify that account, as described above.
          They are not sold, not used for advertising, and not transferred to others except as this policy describes, for example when owners of installations you join see your email address.
        </p>
      </section>

      <section aria-labelledby="privacy-rights" class="mt-10 grid gap-3">
        <h2 id="privacy-rights">
          Your choices
        </h2>
        <ul class="grid list-disc gap-2 pl-5">
          <li>Your account settings show your email address, sign-in methods, active sessions and the audit log. You can remove sign-in methods, revoke sessions and turn notifications off there.</li>
          <li>
            <strong>Delete your account</strong> from Account security in your account settings. Deletion removes your account, sign-in methods, sessions, notification devices, memberships, external app authorizations and pending invitations to your address.
            Installations you own become unclaimed: their data stays on their machines and Beacon keeps their record so they can be claimed again.
            Audit entries that mention your account identifier remain until their 90 days are over, and abuse-protection counters for about two days.
          </li>
          <li>
            For a copy of your data or any other request, write to
            <a class="text-accent underline underline-offset-2" href="mailto:privacy@cairn.build">privacy@cairn.build</a>.
          </li>
        </ul>
      </section>

      <section aria-labelledby="privacy-changes" class="mt-10 grid gap-3">
        <h2 id="privacy-changes">
          Changes
        </h2>
        <p>When Beacon’s handling of personal data changes, this page and its date are updated.</p>
      </section>
    </article>
  </main>
</template>
