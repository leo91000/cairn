# Cairn Android 0.47.0 — Nodes et synchronisation

- La node d’exécution tient dans une ligne compacte. Touchez-la pour voir les fichiers chargés à la demande, les changements non synchronisés et les erreurs éventuelles.
- Choisissez Automatique, Préférer ou Fixer avec une explication adaptée. Enregistrer devient disponible quand la préférence change ; le déplacement dispose de sa propre fiche.
- L’écran Nodes présente la capacité disponible par machine. L’inscription, les accès, la configuration et le stockage ont des formulaires mieux regroupés.
- Le web reprend la même présentation, sur ordinateur comme sur téléphone, avec des tailles de stockage arrondies et une navigation au clavier.

## Cairn Android 0.41.1 — Publication des missions web

- Déploie sur le serveur les missions web de la version 0.41.0, dont la publication avait été bloquée par un test de mise en page instable sous WebKit. Aucun changement pour l’application Android.

## Cairn Android 0.41.0 — Missions web comme sur Android

- Dans l’application web, Missions reprend l’écran Android : une carte par mission avec l’agent, le projet, la planification en clair, la prochaine date, les dernières exécutions et un bouton pour lancer ; les filtres affichent leur nombre de missions.
- La fiche d’une mission s’ouvre à côté de la liste sur grand écran et en feuille sur téléphone : heure planifiée et prochaines dates, mission dépliable, historique avec le taux de réussite, puis Modifier, Mettre en pause et Lancer maintenant. Lancer ouvre l’exécution, comme sur Android.
- La conversation d’une exécution se lit comme un chat : actions résumées en une phrase, frise des étapes et détail de chaque étape dans une fiche. Une exécution s’ouvre sur sa conversation.
- Les réponses de l’agent s’affichent à la taille du texte Android.
- Aucune modification de l’application Android ni du serveur n’est nécessaire.

## Cairn Android 0.40.0 — Conversation web comme sur Android

- Dans l’application web, le contenu d’une conversation reprend celui de l’application Android : vos messages dans une bulle, les réponses de l’agent en texte sous son nom, et une courte conversation collée au-dessus du champ de saisie.
- Les actions de l’agent entre deux messages se résument en une phrase, avec le nombre d’étapes et les échecs ; dépliées, elles forment une frise où les lectures consécutives sont regroupées, et chaque étape s’ouvre sur sa commande, sa sortie et ses détails techniques.
- Les fichiers publiés s’affichent en rail compact sous la réponse. La file d’attente ouvre le champ de saisie, avec « Now » pour le prochain message et ses actions dans une fiche. Le bouton rond du champ envoie, met en file, intervient ou arrête selon le moment, et l’état de connexion flotte au-dessus de la conversation.
- Changer de conversation sur le web ne fait plus clignoter la page : la liste, l’en-tête et le champ de saisie restent en place.
- Android : le geste retour quitte de nouveau une exécution ouverte depuis la fiche d’une mission.
- Aucune modification du serveur n’est nécessaire pour l’application Android.

## Cairn Android 0.39.2 — Historique en défilement infini

- Remonter dans une conversation charge l’historique plus ancien automatiquement, plusieurs écrans à l’avance : plus de bouton « Messages précédents ». Les pages qui ne contiennent presque que des actions de l’agent s’enchaînent jusqu’à ce qu’il y ait assez de texte au-dessus.
- Le texte affiché ne bouge plus quand une page arrive, même pendant un défilement ou un geste rapide. Le groupe d’actions le plus ancien garde sa place quand la page suivante le complète, et une courte conversation reste collée au-dessus du champ de saisie.
- Les réponses de l’agent s’affichent directement à leur hauteur finale en entrant à l’écran, au lieu de s’agrandir après coup.
- Un indicateur discret n’apparaît qu’en haut de l’historique pendant un chargement ; en cas d’erreur, « Réessayer » relance la page.
- L’état de connexion (« Connexion… », « Reconnexion… », erreurs) s’affiche en pastille au-dessus de la conversation, au lieu d’une ligne qui la décalait à chaque reconnexion. Le journal d’une exécution fait de même.

## Cairn Android 0.39.1 — Suivi des réponses

- En suivant un agent qui travaille, un geste vers le haut en bas de la conversation pouvait laisser l’étirement de fin de liste bloqué : les fichiers ne s’ouvraient plus et le balayage vers les conversations voisines ne répondait plus, alors que le défilement vertical fonctionnait. L’étirement revient maintenant toujours en place, et les touchers atteignent de nouveau les fichiers et le changement de conversation. Le journal d’une exécution suivie bénéficie de la même correction.

## Cairn Android 0.39.0 — Comptes des agents de code

- Codex et Claude Code partagent maintenant les mêmes comptes : plusieurs comptes Claude Code sont possibles, avec les mêmes réglages que Codex (pause, exécutions parallèles, reconnexion).
- Connexions présente une carte par agent de code et une ligne compacte par compte : usage restant, un seul badge quand il est utile, et la comète quand le compte travaille. Une fiche détaille l’usage, les réinitialisations en réserve, les exécutions en cours et les réglages.
- Un seul parcours « Ajouter un compte » pour les deux agents. L’Atelier signale quand les agents Codex ou Claude Code attendent un compte pour démarrer.
- Nécessite le serveur 0.39.0, publié avec la même étiquette. Au premier démarrage, le serveur migre une fois les comptes Codex existants et la connexion Claude Code.

## Cairn Android 0.38.6 — Swipe façon Gmail

- Dans le Fil et la liste des conversations, la ligne suit maintenant le doigt sur un fond rouge avec une icône de corbeille. Au-delà de 40 % de la largeur, l’icône grossit et le téléphone vibre.
- Relâcher ou effectuer un geste rapide après ce seuil fait glisser la conversation hors de l’écran ; la liste se resserre. Avant le seuil, la ligne revient en place avec un effet élastique.
- Un snackbar « Conversation supprimée · Annuler » permet de la restaurer immédiatement. Une conversation avec un travail en cours demande toujours confirmation ; en cas d’annulation, elle réapparaît.
- La suppression reste accessible aux lecteurs d’écran par une action « Supprimer » sur la ligne.

## Cairn Android 0.38.1 — Suppression depuis le Fil

- Le swipe vers la gauche révèle maintenant « Supprimer » sur les conversations du Fil Android, dans les sections Pour vous, En cours et Récents. Le swipe vers la droite referme le bouton.
- Le geste seul ne supprime rien. Une conversation avec un travail ou des envois en attente exige toujours une confirmation avant l’arrêt et la mise en corbeille.
- Le test couvre désormais le parcours depuis l’application complète, en plus du sélecteur de conversations.

## Cairn Android 0.38.0 — Application web « Signal »

- L’application web adopte le design de l’application Android : Fil, Missions et Atelier, indicateur « Souffle », avatars animés des agents au travail, palette ⌘K et raccourcis clavier.
- Aucune modification de l’application Android ni du serveur n’est nécessaire.

## Cairn Android 0.37.0 — Archives et corbeille

- Les conversations actives restent au premier plan. Les archives et la corbeille sont accessibles dans une vue secondaire.
- Un swipe vers la gauche révèle « Supprimer ». Une conversation supprimée reste récupérable pendant 30 jours ; arrêter un travail en cours demande confirmation.
- L’archivage automatique S3 et le passage à Glacier sont configurables côté application, après configuration du stockage côté serveur. La restauration d’une archive est explicite.
- Les environnements de travail peuvent utiliser KVM dans Firecracker ; l’option AOSP est proposée pour les tests Android sans services Google Play.

## Cairn Android 0.36.0 — Travail de l’agent plus lisible

- **Indicateur « Souffle »** : pendant que l’agent travaille, la conversation et l’exécution affichent l’étape réelle en cours (par exemple « Exécuter les tests » avec sa commande), un halo qui respire, un reflet sur le texte et la durée à la seconde depuis votre dernier message. Sans étape en cours, l’indicateur rappelle la dernière étape terminée. L’étape est annoncée par TalkBack et l’animation suit le réglage d’animations du système.
- **Actions de l’agent** : repliées, elles se résument en une phrase (« A lu 2 fichiers, cherché 1 fois, modifié 2 fichiers, lancé 1 commande ») avec le nombre d’étapes et d’échecs. Dépliées, elles forment une frise : une ligne par étape, lectures successives regroupées, fichiers modifiés nommés, échecs signalés avec leur code.
- Toucher une étape ouvre son détail complet dans une feuille : commande, sortie, diff ou résultat structuré, bouton « Copier la sortie » et source technique. L’étape en cours n’est plus répétée sous l’indicateur.
- Aucune modification du serveur n’est nécessaire.

## Cairn Android 0.35.0

- Une image copiée peut être collée directement dans le champ de message (appui long → **Coller**) : elle rejoint les pièces jointes du brouillon au lieu d’être ignorée.
- Les images proposées par le clavier (presse-papiers de Gboard, GIF, autocollants) sont aussi ajoutées en pièce jointe.
- Les limites restent celles des pièces jointes : 8 fichiers, 10 Mo par fichier et 40 Mo par message. Le texte collé s’insère comme avant et la surbrillance des `$skills` est conservée.
- Aucune modification du serveur n’est nécessaire.

## Cairn Android 0.34.0 — Nouvelle interface « Signal »

- Trois onglets au lieu de quatre — **Fil**, **Missions**, **Atelier** — dans une barre flottante, avec un bouton « + » toujours accessible pour démarrer une conversation.
- **Fil** remplace la liste des chats : ce qui vous attend (questions, échecs, reconnexions), le travail en cours avec sa durée, puis les conversations récentes. Répondre, relancer une mission ou ouvrir les connexions se fait depuis la ligne.
- **Nouvelle conversation** sans menus déroulants : l’agent et le projet se choisissent directement à l’écran ; les projets respectent les accès de l’agent.
- **Conversation** : en-tête avec l’agent, le statut en direct et les fichiers ; menu d’options allégé. Les messages en attente restent à côté du champ de saisie et peuvent encore être modifiés, envoyés maintenant ou retirés. Le composer garde le même fonctionnement (⚡ intervenir, + à la suite, arrêter).
- **Missions** réunit les anciens onglets Tâches et Activité : planification décrite en clair (« Chaque lundi · 09:00 »), historique des dernières exécutions, lancement direct, fiche détaillée avec prochaines dates, taux de réussite, pause et édition.
- **Atelier** remplace le menu « Plus » : état des connexions Codex, Claude Code et 1Password en premier, puis agents, projets, skills, serveurs MCP, journal des exécutions et paramètres.
- **Recherche** unique sur les conversations, missions, agents, projets et skills, avec actions directes (lancer une mission, discuter avec un agent).
- Nouvelle palette papier/encre avec le bleu Cairn comme unique couleur d’accent, en clair comme en sombre ; identités de couleur pour les agents et les projets. Le sélecteur de modèle est inchangé.
- Aucune modification du serveur n’est nécessaire.

## Cairn Android 0.33.4 — Réponses préservées dans les longues conversations

- Le serveur conserve les messages de l’agent et les erreurs même lorsque les sorties d’outils atteignent leur limite de journalisation.
- Au redémarrage, les conclusions encore disponibles dans les résumés des conversations terminées sont réintégrées à l’historique si elles manquent, sans doublon.
- Le correctif serveur fonctionne avec les applications déjà installées. Les échanges dont aucune copie n’a été sauvegardée ne sont pas reconstitués.

## Cairn Android 0.33.3 — Explication des skills indisponibles

- Taper `$` sans skill accessible affiche « Aucun skill disponible pour cette conversation », avec une indication vers la bibliothèque Skills.
- Une recherche sans résultat affiche un message distinct. Retour ferme le panneau sans effacer le brouillon ; l’envoi reste disponible.

## Cairn Android 0.33.2 — Titres fondés sur toute la conversation

- Le serveur prend en compte tout l’historique pour nommer les conversations, avec des résumés par blocs pour les longues discussions.
- Les simples relances et confirmations ne doivent plus masquer le sujet général. Ce correctif serveur fonctionne aussi avec les applications déjà installées.

## Cairn Android 0.33.1 — Skills avec `$`

Première version Android avec les skills `$` : la 0.33.0 n’a pas été publiée, sa validation automatique ayant échoué sur un test trop pressé.

- Tapez `$` dans une conversation pour afficher les skills disponibles pour l’agent et le projet, filtrés au fil de la saisie avec leur description et leur portée.
- Touchez une suggestion pour insérer `$nom` ; Retour ferme la liste. Les skills reconnus sont surlignés dans le brouillon et dans les messages envoyés.
- Le serveur 0.33.0 demande explicitement à Codex et à Claude Code d’appliquer les skills invoqués, y compris dans un message envoyé pendant une réponse.

## Cairn Android 0.32.0 — Titres de conversation automatiques

- Le serveur adapte les titres au sujet des échanges récents avec GPT-6 Luna en xhigh.
- Le renommage apparaît automatiquement dans les conversations, sans interrompre les réponses.
- Nécessite le serveur 0.32.0 et un compte Codex compatible ; fonctionne aussi avec les conversations Claude.

## Cairn Android 0.31.1 — Reprise des conversations Claude

- Le serveur 0.31.1 corrige les conversations Claude qui restent bloquées après une interruption malgré une reprise demandée.
- L’historique et le travail déjà terminé sont conservés. Ce correctif serveur s’applique aussi aux applications déjà installées.

## Cairn Android 0.31.0 — Sélecteur de dépôts GitHub repensé

- Cartes de dépôts avec avatar du propriétaire, visibilité, langage, étoiles, branche par défaut et dernière activité, triées par activité récente.
- Recherche surlignée sur le nom, la description et le langage ; les pages suivantes se chargent automatiquement pendant la recherche et le défilement.
- Filtres Tous / Privés / Publics avec compteurs, rechargement, et squelettes pendant le chargement.
- Le dépôt choisi se replie en résumé avec un bouton « Changer ». Nécessite le serveur 0.31.0 pour les nouvelles informations.

## Cairn Android 0.30.1 — Conversations en attente

- La conversation indique clairement quand il faut reconnecter Claude Code, avec un bouton vers les connexions.
- Le message en attente reste conservé ; une exécution en file d’attente n’est plus présentée comme un agent qui travaille.
- Les autres motifs d’attente transmis par le serveur sont également visibles dans la conversation.

## Cairn Android 0.30.0 — Mises à jour depuis l’application

- Détection des nouvelles versions au lancement et dans les paramètres.
- Téléchargement vérifié et installation de l’APK via Android.
- Publication automatique d’un APK signé à chaque tag stable `vMAJOR.MINOR.PATCH`, après les contrôles CI.
- Première transition depuis les anciens APK de debug : une désinstallation peut être nécessaire, puis une reconnexion au serveur.

## Cairn Android 0.15.0 — Parallel Claude conversations

- Connections → Claude Code now lets you choose 1–32 simultaneous conversations (default 4), within the server’s total execution capacity.
- Changing the limit takes effect immediately for queued conversations and lets active conversations finish.
- Server 0.29.0 keeps credential refresh on the manager so concurrent conversations cannot overwrite each other’s sign-in state.
- Installable development APK: code 29.

## Cairn Android 0.14.0 — Claude Code usage limits

- Connections shows remaining Claude Code usage for the five-hour, weekly and available model-specific windows, with progress bars and reset dates.
- Last known values stay visible during active runs or temporary errors, with their check date. Reconnecting or disconnecting clears the cached limits.
- Requires server 0.28.0. Installable development APK: code 28.
- Tracks WW-4965.

## Cairn Android 0.13.0 — GitHub project picker

- Projects → Add project → GitHub lists repositories accessible to the shared GitHub connection, including private and organization repositories.
- Filter loaded repositories, load additional pages, see already registered repositories and retry connection errors.
- Selecting a repository fills its name, description and default branch. Saving clones it automatically into a managed server workspace; manual server paths remain available.
- Requires server 0.27.0. Installable development APK: code 26.

## Cairn Android 0.10.2 — Fresh model catalogs

## 0.11.0

- Choose Codex or Claude Code directly in the composer or chat preferences.
- Continue in the same chat and workspace after a provider switch (server 0.25.0).
- Reset incompatible model/effort defaults, preserve provider selection through recreation, and retain the provider when editing queued messages.
- Accept Claude model aliases with context suffixes such as `opus[1m]`.


Opening model or reasoning settings reloads the selected provider's catalog, including in an existing chat. The open picker also refreshes on foreground return and every five minutes. **Actualiser les modèles** retries without changing the selected model or effort. Network failures retain the last catalog, including for Claude. Installable development APK: code 23.

Server 0.24.2 updates the manager and guest Codex CLI to 0.156.1, whose catalog includes GPT-6 Sol and Luna. Actual options still come from the connected accounts, without hard-coded additions or an inference request.

## Previous release: Cairn Android 0.10.1 — Configurable parallel runs

Codex accounts now accept parallel-run limits above four in Connections. The minimum remains one. Requires Léo server 0.24.1, whose global `CONCURRENCY` also accepts values above four (default four). Installable development APK: code 22.

## Previous release: Cairn Android 0.10.0 — Claude Code

Connect Claude Code through its official browser sign-in, finish with an authorization code, and cancel or retry from Connections. Choose Codex or Claude Code per agent; chat model and effort choices follow that provider. Existing agents remain on Codex. Requires Léo server 0.24.0.

## Previous release: Cairn Android 0.9.0 — model and reasoning controls

- Model and reasoning buttons are directly visible in the chat composer.
- Search the server model catalog in a native sheet. Changing models resets the
  reasoning override; agent/model defaults remain explicit and recoverable.
- Reasoning uses a rounded discrete slider with French labels, haptic feedback,
  accessibility actions and only the levels supported by the selected model.
- Settings survive draft navigation and recreation and apply to the next message.
- Agent editing uses the same controls; tasks continue to inherit their agent.
- No server changes or migration required. Installable development APK: code 20.

## Previous release: Cairn Android 0.8.0 — 1Password service accounts

- Add multiple named service account tokens from Connections → 1Password.
- Explicitly allow or deny each agent, including Main; access starts disabled for every agent.
- Rotate tokens, disable or delete accounts, and test the connection.
- Tokens are masked during entry, encrypted on the server, and never returned to the app.
- Requires server v0.23.0 with the scoped read-only 1Password workspace tool.

## Previous release: Cairn Android 0.7.0 — public artifact links

- Enable or revoke a public link from the artifact viewer. Files stay private by default.
- Copy the link or send it using Android’s native share sheet. Recipients need no account.
- Sharing applies to the selected version only; new versions remain private.
- Re-enabling a revoked link creates a different URL. Copies already downloaded remain with recipients.
- Retains the Fil design and floating latest-message control. Requires the accompanying server update.

## Previous release: Cairn Android 0.6.2 — floating latest-message control

- The jump-to-bottom arrow floats at the top right of chat and task conversations.
- It fades out during scrolling, returns once scrolling stops when newer content is below,
  and resumes following on tap. It no longer reserves a row above the composer.
- Compact 36 dp circular face, 48 dp touch target and the existing web palette.

## Previous release: Cairn Android 0.6.1 — Fil, compact native reading

- Implements the selected Fil direction using the web palette and bundled DM Sans /
  Manrope fonts. Font licenses are included in the APK; Android font scaling is retained.
- Compact 56 dp conversation header and single-row resting composer. Messages use
  16 sp body text, 24 sp line height and 16 dp reading margins; assistant replies
  stay on the page rather than in cards.
- Sending uses a discreet 32 dp face and 18 dp arrow inside a 48 dp native touch target.
- Published files use a compact horizontal rail with small previews in the transcript.
  Full galleries, group names, versions, authenticated opening and sharing remain available.
- Tasks have an underlined filter strip, flat rows, fine separators and a narrow
  selection marker. Execution details remain expandable with labelled touch targets.
- History follow distinguishes actual finger movement from text relocation during a stationary press.
- No backend change or dependency-pin upgrade. This is a density adaptation inspired
  by the supplied ChatGPT screenshot, not a pixel-identical reproduction.

## Previous release: Cairn Android 0.6.0 — native web parity

- Compact Conversations header with a searchable, dated native conversation sheet.
  Switching conversations retains message, model and attachment drafts.
- Ordinary sends appear in the transcript. The waiting queue is collapsible;
  steering, pause, edit, remove and stop remain available.
- Agent-reported completion appears below the reply with expandable evidence.
  Blocked and input-required reasons remain visible. Routine session notices stay hidden.
- Tasks use a grouped inbox, a compact chooser on phones and a side pane on large
  windows. Conversation, Result and Files have native tabs. Task and execution
  details are sheets; management and recovery actions remain available.
- Fullscreen reading, selectable messages, consistent Markdown font scaling,
  compact Cairn navigation and search across tasks, agents, projects and skills.
- Project source mode and starting revisions match the web. Agent access editing
  matches shared GitHub restrictions, dedicated tokens and MCP tool selection.
- History responses capture the latest visible message before inserting older
  rows, protecting the reading anchor when a response overtakes a scroll observer.
- Existing encrypted history, stable scrolling, files, questions, connections and
  native authentication flows are retained. No backend migration is required.

Validation is recorded separately in [WEB-PARITY.md](docs/WEB-PARITY.md) and
[VALIDATION.md](VALIDATION.md).

## Previous release: Cairn Android 0.5.6 — stop cascading history loads

- Loading an older page waits for the viewport to settle before automatic paging
  can re-arm. A response alone no longer triggers the next page.
- The visible text anchor is retained when a page arrives during an active drag;
  the same finger can continue scrolling without lifting.
- Includes the latest chat improvements hiding routine session notices.
- No server change or database migration is required.

## Previous revision: Cairn Android 0.5.5 — stable older-history loading

- Loading older messages preserves the current reading position, including movement
  made while the request is pending, in both chats and runs.
- Long Markdown messages reserve space during their initial asynchronous render so
  the list does not skip them before their text appears.
- The older-history control keeps a stable height. Automatic paging uses distance
  from the top and requests each cursor once; explicit error retry remains available.
- No backend change or database migration is required.

## Previous revision: Cairn Android 0.5.4 — message links and artifact previews

- Markdown links open from the conversation while keeping text selectable.
- Artifact links open the authenticated native viewer, including files from older
  runs that are no longer in the loaded history. Relative and same-server absolute
  links are supported; external links use the browser.
- Missing files and browser-opening failures produce a visible message.
- The web client also opens artifact message links in its integrated viewer.

## Previous revision: Cairn Android 0.5.3 — predictable conversation following

- The down arrow reaches the actual bottom, including the final spacing.
- Touch pauses automatic scrolling immediately. Moving toward older text exits
  follow, including while the assistant is streaming.
- Scrolling toward the end keeps follow enabled; reaching the end manually enables
  it again. Overscroll at the bottom no longer disables follow.
- A tap, incoming text or keyboard resize never pulls a reader back down.
- Chats and run activity use the same gesture handling.

## Previous revision: Cairn Android 0.4.1 — stable conversation resume

- Open conversations retain their stream cursor and accumulated messages while the
  screen is stopped. Returning to the foreground resumes from the accepted cursor.
- Initial history and reset/reconnect catch-up are displayed atomically, after the
  last batch. The conversation no longer jumps through old pages while loading.
- A restored list is attached only when its history is ready. Manual reading
  position and paused following survive background/foreground transitions.
- Chat and run auto-follow wait for history readiness. Stream state remains scoped
  to the connection and route, and is reset when the session generation changes.
- Regression tests exercise partial pages, opening/reopening, foreground resume,
  manual scrolling, compressed message continuation and session/path isolation.
- The native Android client, CI workflow and native MCP OAuth server bridge are
  included together in the repository. Generated Android builds are excluded from
  the web linter.

## Previous revision: Android 0.4.0 — native activity presentations

- Session lifecycle events show readable French summaries instead of raw event JSON.
  Account selection and successful completion use compact rows. Raw source is
  available only through the advanced disclosure for normal-sized supported results.
- Terminal commands, file reads, workspace browsing, searches, diffs, plans,
  reasoning and MCP calls each have a native presentation with an icon and status.
- Simple shell commands are classified without execution. Shell operators,
  substitutions and ambiguous commands stay in the terminal view. Expected
  grep/rg and diff exit-code outcomes are distinguished from failures.
- MCP text/structured results become readable fields, expandable collections,
  workflow-check rows and resource links. Returned images use bounded native
  decoding. Unknown result fields remain inspectable; truncated JSON is labelled.
- Markdown reads have a document preview, file changes show coloured diffs and
  addition/removal counts, and plans show progress and completed steps.
- Legacy recorded tool steps are paired with their results. New and historical
  activity share these components in chats and runs.
- JSON artifacts also have a structured reader with source available separately.
  Inline artifact viewers use the conversation host so live updates do not close them.

The web activity modules in this repository are the reference. No API, dependency,
notification, authentication or server deployment changes are part of this revision.

## Previous revision: 0.3.0 — conversation-focused interface

## Changes

- Chat and run details have a single compact header. Phone bottom navigation is
  reserved for the main sections, freeing space for the conversation and keyboard.
- The chat composer grows to at most four visible lines. Attach and send/stop use
  icons; model, reasoning, pause and run navigation are available from the header.
  Steering is available in that menu when composing during an active run.
- Assistant messages flow directly on the page; user messages use a quiet,
  right-aligned bubble. Markdown has more generous line spacing.
- Consecutive technical events form collapsed activity groups. Tool lifecycle
  updates merge at their original position within a turn. Errors remain signalled
  on the collapsed group; commands, results and raw data remain inspectable.
- Published files appear in grouped horizontal galleries beside the relevant
  response, following the web timeline, and in the run result. The full gallery provides search, version selection and an adaptive
  grid. File readers use compact download/share/open-with actions.
- Run details concentrate on result and activity, with files and mission details
  accessible from the header. Dragging upward stops automatic following; a
  labelled arrow returns to the latest activity.
- Main screens use quieter headings, search fields and cards. Task, agent,
  project, skill and MCP actions use labelled icon controls with tooltips and
  native touch targets. Confirmation flows remain in place.

The app retains Cairn's blue-violet light/dark theme and native Compose components.
No backend or authentication changes are part of this visual revision.
Notifications continue to use the previously selected Firebase-free periodic
checks. The optional native MCP OAuth server patch from 0.2.0 is unchanged and
has not been deployed by this task.

## Validation

See `VALIDATION.md` for measured checks and device limitations. Captures are
native Robolectric renders with fixture data; a constrained viewport exercises
reduced available height without claiming to emulate a Samsung keyboard.
