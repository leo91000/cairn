# Les tours de plan sont bornés par Cairn, pas par le modèle

Date : 2026-10-24.

## Constat

Un utilisateur veut parfois faire explorer un sujet et proposer une approche avant
d’autoriser l’implémentation, puis passer à l’exécution dans le même fil. La barre
de progression des conversations (`todo_list`, `agent_plan.rs`) ne répond pas à ce
besoin : elle suit le travail d’un agent qui modifie déjà les fichiers.

Les deux moteurs ont un mode de planification, mais aucun ne suffit seul :

- Claude Code 2.1.280 a `--permission-mode plan`. Il demande l’autorisation des
  modifications à notre callback, mais écrit lui-même son fichier de plan, et le
  callback actuel de Cairn autorise `Bash` et tous les outils MCP. Ajouter le mode
  seul laisserait passer des effets par le shell et les outils MCP.
- Codex 0.164.0 expose des préréglages (`collaborationMode/list`, expérimental) et
  `turn/start.collaborationMode`. Ce mode change les consignes, pas les droits. Une
  mesure sur 0.164.0 montre aussi qu’un `thread/resume` qui demande
  `sandbox: read-only` garde le bac à sable `workspace-write` du premier tour : seul
  `turn/start.sandboxPolicy` le change, et Codex le conserve pour les tours suivants.
  Le mode de collaboration persiste de la même façon.

## Décision

Un message porte `plan: true` pour demander un **tour de plan**. Son absence, comme
chez les anciens clients, garde le comportement existant. Le serveur refuse un tour
de plan pour un moteur dont il ne sait pas borner les effets (OpenCode, qui n’a pas
de bac à sable), et refuse de faire rejoindre un tour en cours qui peut déjà modifier
les fichiers (`steer`). Les missions et les reprises planifiées ne sont jamais des
tours de plan. Le drapeau est une colonne typée de `chat_messages`, et de
`chat_questions` pour les questions posées par un tour de plan, ajoutée par la
[migration 22 → 23](../migrations/0023-plan-requests.md), afin qu’une demande en file
ou la réponse à une telle question garde sa frontière après un redémarrage.

Pendant un tour de plan, Cairn applique lui-même la frontière :

- **Bac à sable du moteur en lecture seule, sans réseau pour le shell**, quels que
  soient les droits de l’agent, y compris YOLO. Pour Claude Code : bac à sable
  `failIfUnavailable`, sans repli hors bac à sable, aucun domaine réseau et une liste
  stricte (`network.strictAllowlist`) qu’aucune commande ne peut élargir par ses
  propres `allowed_domains`, et des règles `Edit(//racine/**)` qui refusent toute
  écriture dans l’espace de travail avant tout mode ou callback. Pour Codex :
  `turn/start.sandboxPolicy` en `readOnly` sans réseau, y compris sur un fil créé en
  YOLO. Les sous-agents des deux moteurs restent dans cette frontière : Codex 0.164.0
  propose encore ses outils de collaboration malgré `features.multi_agent = false`,
  et Claude Code lance ses sous-agents sans demander le callback. Codex garde le bac à
  sable de chaque sous-agent qu’un processus a chargé, même après `thread/unsubscribe`
  du parent : un sous-agent qu’un tour YOLO a lancé pouvait écrire et utiliser le
  réseau quand un tour de plan lui confiait une suite. Le service Codex résident de
  Cairn démarre donc un nouveau processus natif pour un tour de plan et pour le tour qui
  le suit ; un sous-agent rechargé y prend le bac à sable du tour. S’il ne peut pas en
  démarrer un, le tour échoue sans s’exécuter. Hors service résident, chaque tour
  démarre déjà son propre processus, comme Claude Code. Le champ
  `sandbox: read-only` du plan reste compris par les adaptateurs antérieurs.
- **Callback Claude restreint** : lecture, recherche, shell dans le bac à sable sans
  `allowed_domains`, outils MCP déjà restreints, et le seul fichier de plan sous l’état
  privé de Claude Code. Claude Code 2.1.308 confie sinon les décisions du mode plan à
  son classificateur du mode automatique, un modèle qui peut approuver une commande
  et ses domaines : `useAutoModeDuringPlan: false` et
  `permissions.disableAutoMode` rendent la décision au callback de Cairn. `ExitPlanMode` est refusé : l’accepter laisserait Claude implémenter dans le
  même tour. Le modèle écrit son plan dans le fichier de plan puis appelle
  `ExitPlanMode` sans argument ; Claude Code (2.1.280, 2.1.308) ajoute alors le contenu
  du fichier (`plan`) et son chemin à l’appel. Ce plan est affiché comme **plan
  proposé**, comme l’élément `plan` de Codex. Sans fichier de plan, aucun plan vide
  n’est affiché et Claude est invité à l’écrire avant de réessayer. Seul un tour de plan
  de Cairn affiche un plan proposé, avec « rien n’a été modifié » et **Implémenter** :
  dans un tour ordinaire, le mode plan propre à Claude Code peut implémenter juste
  après `ExitPlanMode`, qui s’affiche alors comme un appel d’outil, et un élément
  `plan` de Codex comme un raisonnement. Les droits du tour ordinaire ne changent pas.
- **Outils MCP en lecture seule seulement** : pour une connexion, les outils qu’elle
  déclare `readOnlyHint`, appliqués par la passerelle (connexions HTTP) et par la
  liste d’outils du moteur (connexions par commande) ; pour `cairn_installation`, les
  opérations de portée `read` ; pour `cairn_workspace`, `open_project`, `list_nodes`,
  `exclusive_status`, `ask_questions`, `pending_questions`, `withdraw_question` et la lecture des tâches déléguées
  (`task_status`). Les outils et commandes des plugins sont refusés, comme
  `delegate_task`, `task_cancel`, `wait_for`, `publish_artifact` ou `report_outcome`. Ces restrictions
  suivent l’autorisation du tour (`chatExecution.plan`), que le tour suivant remplace.

Les préréglages du moteur ne servent qu’à guider le modèle. Cairn découvre ceux de
Codex à chaque tour et envoie `plan`, puis `default` au tour qui suit un tour de plan,
avec le modèle du message ou, à défaut, celui que Codex applique au fil (sa
configuration peut le fixer), jamais le modèle par défaut de son catalogue à sa place.
La reprise d’un tour de plan déjà terminé rejoue son plan, même sans autre réponse.
Si Codex n’en propose pas, le tour reste borné et reçoit seulement la consigne de
Cairn.

Le passage à l’implémentation est explicite : un message ordinaire dans le même fil,
envoyé depuis le composeur ou le bouton **Implement** / **Implémenter** du dernier
plan proposé. Il s’exécute avec les droits courants de l’agent, contrôlés comme
avant par `covers` : il n’élargit aucun droit. Le bouton transmet `planApproval`
(identité `runId:eventId` de l’événement principal achevé et révision du fil). Dans la transaction
qui accepte le message, Cairn exige cette révision, la dernière proposition
principale et l’absence de demande en attente ou de tour actif. Une approbation
périmée échoue en conflit et les clients rafraîchissent le fil ; elle ne devient
jamais une demande ordinaire sans précondition. Un retry du même message déjà
accepté garde son résultat idempotent. Cette précondition transitoire ne change ni
le format des messages persistés ni les messages écrits librement au composeur.
Les interfaces MCP publique et installation utilisent le même contrat d’envoi.

Un plan proposé est un contenu de conversation, pas une activité d’outil : il traverse
le transport et le stockage du manager sans l’aperçu de 16 Kio des outils (seulement
masqué comme une réponse), et l’historique transmis à une nouvelle session (autre
moteur, nouvelle session, environnement expiré) l’inclut, borné comme les réponses.
Implémenter après un changement de moteur reçoit donc le plan à appliquer.
Les propositions des sous-agents restent leurs étapes attribuées : elles ne
remplacent pas le plan principal, ne portent pas son bouton d’implémentation et
ne sont pas transmises comme propositions de l’agent principal au nouveau moteur.

## Qualification

`crates/installation/tests/native_plan_turns.rs` fait exécuter un tour de plan aux
vrais binaires, à travers les adaptateurs de Cairn, face à un modèle local scripté qui
demande des effets. Aucun identifiant ni service externe n’est utilisé ; toutes les
écritures visent un répertoire temporaire. Ignorés dans la suite ordinaire, ces tests
sont lancés par le job `backend` du workflow complet (chaque nuit, sur dispatch manuel et
pour chaque tag de version), pour les versions de Claude Code et de Codex que le build
résout ; `publish` dépend de ce job : une version dont la frontière ne tient pas n’est
pas servie. La validation courte des PR ne les lance pas, ni un dispatch `backend-only`,
qui ne résout aucune version. Résultats du 2026-10-27 :

- Claude Code 2.1.308 et 2.1.280 : écritures du shell dans l’espace de travail, le
  HOME et `/tmp` refusées (système de fichiers en lecture seule), connexion locale
  refusée, hôte non autorisé refusé par le proxy (403), `allowed_domains` refusé,
  `dangerouslyDisableSandbox` refusé par le callback, `Write`/`Edit` refusés, et un
  sous-agent démarré par `Agent` soumis aux mêmes refus. Le plan écrit dans le fichier
  de plan, puis `ExitPlanMode` sans argument, est le plan proposé affiché. Avant
  `strictAllowlist`, un classificateur qui approuvait la commande laissait passer
  l’hôte de ses `allowed_domains` jusqu’au proxy (502).
- Codex 0.164.0, sur un fil créé par un tour YOLO : écritures dans l’espace de travail
  et le HOME refusées, connexion locale refusée, demande d’escalade refusée ; le
  sous-agent lancé par `spawn_agent` a tenté d’écrire et de se connecter, et ses deux
  tentatives ont été refusées ; à travers le service résident, un sous-agent lancé par
  un tour YOLO, relancé pendant le tour de plan, a vu son écriture et sa connexion
  refusées, puis a écrit au tour d’implémentation ; tous les tours gardent le modèle configuré ; le tour
  précédent et le tour qui suit le plan écrivent normalement.

La fraîcheur suit les événements persistés, pas la position de leur carte repliée.
Une nouvelle proposition principale, même vide ou portant le même identifiant,
invalide la précédente dès son début ou sa mise à jour. Seul son événement final
avec du contenu peut être approuvé. Une clarification reçue avant cette fin en
fait partie ; un message reçu après l’invalide. Web, Android et la transaction
commune HTTP/MCP appliquent cet ordre, y compris après chargement d’une ancienne
page. Les clés visuelles restent ancrées pour conserver la position de lecture.

Chaque proposition terminée conserve aussi l'identifiant de la dernière demande
humaine connue lors de sa complétion (`request_id`). Le reçu Claude le garde avec
l'objet ; le rejeu Codex le reconstruit dans l'ordre des éléments natifs, avant les
clarifications ultérieures. Un nouvel identifiant d'événement lors de la reprise
ne renouvelle pas cette frontière : clients et transaction d'approbation refusent
une demande plus récente. Les anciens reçus à une seule demande ont une frontière
non ambiguë ; ceux à plusieurs demandes sans cette information restaurent le
contenu mais exigent une nouvelle proposition avant l'approbation. Cela ne relance
pas un tour natif achevé et ne modifie pas les messages ordinaires.

## Limites

- `readOnlyHint` est déclaré par le serveur MCP. Un serveur qui le donne à tort à un
  outil à effets peut agir pendant un tour de plan ; un outil sans annotation est
  refusé.
- Les commandes qui écrivent (compilation, tests) échouent pendant un tour de plan.
- Claude Code exige bubblewrap et socat pour son bac à sable. Une node qui ne les a
  pas refuse le tour de plan avec une erreur explicite, sans rien exécuter.
- Le refus du réseau s’applique au shell. `WebFetch` et la recherche web restent
  disponibles en lecture.
- Sur une node native dont le processus de travail garde une capacité héritée, ou
  dont `/tmp` est monté depuis un répertoire inaccessible, bubblewrap refuse de
  démarrer : les commandes du shell échouent sans effet pendant le tour de plan.
  La qualification ci-dessus retire ces capacités. Si un `/tmp` privé est nécessaire,
  il entoure le runner de tests entier : chaque Session supervise ainsi directement
  le binaire natif. Un wrapper bubblewrap par processus peut se terminer avant son
  enfant et laisser son writer actif à la reprise ; ce montage n’est pas utilisé.
- La CI qualifie les versions qu’elle résout, sur un exécuteur Ubuntu où bubblewrap
  et socat sont installés et où les espaces de noms utilisateur non privilégiés sont
  permis ; elle ne mesure pas chaque configuration de node. OpenCode refuse les tours
  de plan et n’est pas qualifié.

During Plan, `ask_questions`, `pending_questions` and `withdraw_question` keep the
common questionnaire lifecycle available. Withdrawal only retires a pending
question owned by the current execution and conversation; it never supplies an
answer or authorizes implementation. Questions answered in ordinary chat or made
unnecessary by exploration should be withdrawn. Delegation and tools with external
effects remain unavailable.
