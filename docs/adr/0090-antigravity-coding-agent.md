# Antigravity par la CLI `agy`, bornée par des hooks de Cairn

Date : 2026-11-22.

## Constat

Google publie la CLI Antigravity (`agy`) sans paquet npm : un installeur lit un
manifeste par plateforme (`version`, `url`, `sha512`) sur le service de mise à jour
de Google. Elle offre un mode sans terminal (`-p`) en NDJSON (`--input-format
stream-json --output-format stream-json`), la reprise par `--conversation <id>`,
des réglages par session (`--settings`), un serveur ACP et un sandbox de terminal.
Les mesures suivantes ont été faites sur 1.5.14 avec un faux serveur Gemini local
(`GOOGLE_GEMINI_BASE_URL`) et une clé synthétique, sans compte :

- Tout l'état natif (conversations, réglages, MCP, hooks, jetons sans D-Bus) suit
  `HOME`, sous `~/.gemini`. Aucune variable ne déplace ce répertoire seul.
- Un processus `stream-json` enchaîne les messages : un message envoyé pendant un
  tour attend la fin du tour et produit son propre `result`. `--model <id>
  --effort <niveau>` choisit le modèle ; un niveau inconnu échoue sans repli.
- `--mode plan` avec `--sandbox` et `--dangerously-skip-permissions` laisse le
  shell et `write_file` écrire dans l'espace de travail ; le sandbox refuse
  seulement l'accès direct par IP. La règle `deny` `command` n'a aucun effet,
  `command(*)` refuse le shell. Les règles `deny` s'appliquent même en
  `always-proceed` ; une règle `ask` n'arrête rien dans ce mode : l'outil s'exécute.
- Les commandes passent par bash, chacune dans sa propre session. Interrompu (y
  compris par SIGTERM), `agy` les laisse tourner.
- `agy` reprend une conversation à partir de sa seule base
  (`conversations/<id>.db`) dans un HOME neuf, y compris quand ce dossier est un lien
  symbolique, et ne charge pas `~/.agents`. Il
  charge en revanche des plugins et réglages de son état, qu'un tour précédent peut
  modifier.
- `agy` charge `.agents/hooks.json` et `.agents/mcp_config.json` du répertoire de
  travail ou de la racine git. Les commandes de ces hooks s'exécutent même quand le
  hook de Cairn refuse l'outil ; `trustedWorkspaces` n'y change rien et aucun réglage
  ne les désactive.
- Un hook `PreToolUse` reçoit le nom et les arguments de chaque outil, y compris
  `call_mcp_tool` (`ServerName`, `ToolName`). Un hook qui échoue, dépasse son délai
  ou répond un JSON invalide refuse l'outil.
- Les conditions d'utilisation (§6) et la FAQ interdisent d'utiliser une connexion
  Google grand public depuis une application tierce et recommandent une clé API.

## Décision

Enregistrer `antigravity` comme agent de code avec un adaptateur `stream-json` :
un processus natif par exécution, réutilisé pour les messages qui rejoignent le
tour. Les messages sont attribués dans l'ordre des étapes `user_input`, et chaque
`result` termine le message suivant de la file. Un reçu durable rejoue un tour
terminé sans nouvelle requête au modèle. L'annulation et la fin de l'exécution
arrêtent l'arbre de processus d'`agy` : Cairn retrouve dans `/proc` ses descendants,
dont les sessions de ses commandes, et tue leurs groupes. La session native est l'identifiant de conversation (UUID) ; la reprise
exige le même répertoire de travail et `--conversation`.

L'état natif vit dans le HOME privé du run (`home/.antigravity`), passé comme
`HOME` au seul processus `agy` : les autres outils gardent le HOME du shell. Cairn
écrit les réglages de chaque lancement par `--settings` (`modelProvider`, règles
`deny`) et remet à zéro le `settings.json` natif ; `AGY_CLI_DISABLE_AUTO_UPDATE`
empêche les mises à jour. Le mode sans terminal de 1.5.14 n'a aucun réglage pour
couper la télémétrie (seul `agy acp --disable-telemetry` existe) : les conditions de
Google s'appliquent aux interactions. Cairn écrit aussi la configuration
MCP dans `~/.gemini/config/mcp_config.json` et ses hooks dans
`~/.gemini/config/hooks.json`. Les réglages du projet (`.agents/`) restent ceux du
dépôt, comme pour les autres moteurs.

**Le hook de Cairn est la frontière.** Il est le binaire Cairn lui-même
(`cairn antigravity-hook`), qui lit sa politique dans un fichier du HOME privé :

- tour ordinaire : tout est permis sauf l'outil natif `ask_question`, remplacé par
  le questionnaire commun ([ADR-0074](0074-common-agent-question-tool.md)), et les
  outils MCP hors des listes autorisées de Cairn ;
- tour de plan : liste blanche en lecture seule (lecture de fichiers, listes,
  recherches, outils Cairn déjà restreints par l'autorisation du tour). Le shell,
  les écritures, les sous-agents, la planification, les workflows et tout outil
  inconnu sont refusés. Les règles `deny` (`command(*)`, `write_file(*)`, URL)
  doublent le hook. Contrairement à Claude Code et Codex, **un tour de plan
  Antigravity n'a pas de shell** : `agy` n'a aucun sandbox en lecture seule ;
- question à côté : tout outil est refusé.

Un tour de plan reçoit aussi `--mode plan` pour guider le modèle ; ce mode n'est
pas une frontière. Un tour de plan tourne dans un HOME neuf : rien de ce qu'un tour YOLO a écrit dans
l'état natif (plugins, réglages) ne s'y applique. Seul son dossier
`conversations` est un lien vers celui de l'état natif, où `agy` reprend et
enregistre directement la conversation : rien n'est recopié, et un tour de plan
arrêté ne perd rien. Un HOME de tour de plan qui ne peut pas être effacé fait
échouer le tour. Un tour de plan ou une question à côté est refusé, avec le chemin en cause, quand
le répertoire de travail ou l'un de ses parents contient `.agents/hooks.json` ou
`.agents/mcp_config.json` : ces commandes du dépôt ne peuvent pas être bornées.

**Questions à côté.** `/btw` n'existe que dans l'interface interactive. L'adaptateur
copie la seule base de la conversation dans un HOME temporaire neuf, lance un second
`agy --conversation <id>` avec le hook qui refuse tout, puis supprime la copie.
Le processus principal et son historique ne sont pas touchés. La réponse voit le
contexte enregistré jusqu'à la dernière étape terminée, pas l'étape en cours.

**Comptes.** Un compte Antigravity est l'un de :

- une clé API Gemini (`GEMINI_API_KEY`, `modelProvider: gemini`) ;
- des identifiants Google Cloud (fichier JSON de compte de service ou
  `authorized_user`, projet et région), installés comme fichier privé désigné par
  `GOOGLE_APPLICATION_CREDENTIALS` avec `AGY_ADC_AUTH=true` ;
- un compte Google connecté par `agy acp --login` : Cairn affiche le lien de Google
  et transmet le code collé, comme pour Claude Code. Les conditions d'utilisation
  d'Antigravity interdisent cet usage depuis une application tierce ; le web et
  Android l'affichent avant la connexion, et le propriétaire en accepte le risque.

Les secrets restent dans le coffre chiffré et ne passent que par le courtier
existant du run. Les identifiants Google Cloud sont fournis tels quels au run : un
fichier `authorized_user` contient donc son refresh token, choisi par l'utilisateur.
Le manager liste les modèles dans un répertoire privé temporaire, supprimé ensuite. Sur le nœud d'exécution, le fichier d'identifiants Google Cloud et le jeton d'accès sont supprimés à la fin de chaque lancement, annulation comprise : le HOME du run peut être un disque durable de node native ou de microVM. Mesuré avec un compte de test : sans D-Bus, `agy` écrit le jeton
OAuth (format `oauth2.Token` : `access_token`, `refresh_token`, `expiry`) dans
`~/.gemini/antigravity-cli/antigravity-oauth-token`, et fonctionne avec le seul jeton
d'accès. **Le refresh token reste sur le manager** : un run reçoit un fichier sans
lui. Pour renouveler le jeton d'accès, le manager écrit le jeton complet, marqué
expiré, dans un répertoire privé temporaire et y lance `agy models` ; `agy` le
rafraîchit lui-même, et Cairn n'embarque jamais le secret client OAuth de la CLI.
Un processus `agy` ne relit pas son fichier : quand son jeton expire pendant un tour,
l'étape échoue (`token expired and refresh token is not set`). L'adaptateur obtient
alors un nouveau jeton du courtier et relance la même conversation avec une consigne
de reprise, au plus 48 fois par tour.

L'usage est indisponible : `agy` n'expose le quota qu'en texte interactif, et pas
du tout avec une clé API. Une erreur 429 utilise l'épuisement
partagé ; le compte redevient éligible après cinq minutes.

**Catalogue.** `agy models` liste des identifiants qui incluent le niveau
(`gemini-4-neon-high`). Cairn les regroupe en un modèle et ses niveaux de
raisonnement. Au lancement, Cairn résout le modèle et le niveau choisis dans le
catalogue du type de compte loué et passe l'identifiant natif
(`--model gemini-4-neon-high`). Le catalogue dépend du type de compte ; un agent
sans modèle prend le premier modèle du type de compte loué.

**Accès.** Comme OpenCode, Antigravity exige l'accès YOLO : son sandbox ne
correspond ni à la lecture seule ni à l'écriture dans l'espace de travail de Cairn.

**Version.** Chaque build résout la version courante du manifeste officiel
`linux_amd64` (l'image est x86_64 uniquement), vérifie son SHA-512 et installe exactement cette
version ; `AGY_CLI_DISABLE_AUTO_UPDATE=true` empêche toute mise à jour. La
qualification CI exécute le binaire résolu contre un faux serveur Gemini.

## Limites

- Le binaire (environ 260 Mo) ne tient pas dans le budget de 1 Gio du bundle
  d'outils des nodes natives, imposé par les superviseurs déjà installés. Il est
  livré dans un second bundle natif qui ne contient que `agy`, avec son propre
  budget ; un superviseur antérieur ne le connaît pas et ses nodes refusent
  Antigravity.
- Le format du fichier de jetons n'est pas documenté par Google. Il a été mesuré sur
  1.5.14 avec un compte de test ; la fixture de Cairn le reproduit. La qualification CI
  ne peut pas le vérifier sans compte Google : une nouvelle version d'`agy` qui le
  changerait ferait échouer les comptes Google (« Reconnect »), pas les autres.
- Pas de compaction demandée par Cairn ni de vitesse.
