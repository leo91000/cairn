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
tours de plan.

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
  et Claude Code lance ses sous-agents sans demander le callback. Le champ
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
  n’est affiché et Claude est invité à l’écrire avant de réessayer.
- **Outils MCP en lecture seule seulement** : pour une connexion, les outils qu’elle
  déclare `readOnlyHint`, appliqués par la passerelle (connexions HTTP) et par la
  liste d’outils du moteur (connexions par commande) ; pour `cairn_installation`, les
  opérations de portée `read` ; pour `cairn_workspace`, `open_project`, `list_nodes`,
  `exclusive_status`, `pending_questions` et la lecture des tâches déléguées
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
avant par `covers` : il n’élargit aucun droit.

## Qualification

`crates/installation/tests/native_plan_turns.rs` fait exécuter un tour de plan aux
vrais binaires, à travers les adaptateurs de Cairn, face à un modèle local scripté qui
demande des effets. Aucun identifiant ni service externe n’est utilisé ; toutes les
écritures visent un répertoire temporaire. Ignorés dans la suite ordinaire, ces tests
sont lancés par le job `backend` de la CI pour les versions de Claude Code et de Codex
que le build résout, et `publish` dépend de ce job : une version dont la frontière ne
tient pas n’est pas servie. Résultats du 2026-10-27 :

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
  tentatives ont été refusées ; tous les tours gardent le modèle configuré ; le tour
  précédent et le tour qui suit le plan écrivent normalement.

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
  La qualification ci-dessus retire ces capacités et donne à Codex un `/tmp` privé.
- La CI qualifie les versions qu’elle résout, sur un exécuteur Ubuntu où bubblewrap
  et socat sont installés et où les espaces de noms utilisateur non privilégiés sont
  permis ; elle ne mesure pas chaque configuration de node. OpenCode refuse les tours
  de plan et n’est pas qualifié.
