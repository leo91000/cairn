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
  `failIfUnavailable`, sans repli hors bac à sable, aucun domaine réseau, et des
  règles `Edit(//racine/**)` qui refusent toute écriture dans l’espace de travail
  avant tout mode ou callback. Pour Codex : `turn/start.sandboxPolicy` en
  `readOnly` sans réseau, et les sous-agents désactivés (`features.multi_agent`).
  Le champ `sandbox: read-only` du plan reste compris par les adaptateurs antérieurs.
- **Callback Claude restreint** : lecture, recherche, shell dans le bac à sable,
  outils MCP déjà restreints, et le seul fichier de plan sous l’état privé de Claude
  Code. `ExitPlanMode` est refusé : l’accepter laisserait Claude implémenter dans le
  même tour. Son plan est affiché comme **plan proposé**, comme l’élément `plan` de
  Codex.
- **Outils MCP en lecture seule seulement** : pour une connexion, les outils qu’elle
  déclare `readOnlyHint`, appliqués par la passerelle (connexions HTTP) et par la
  liste d’outils du moteur (connexions par commande) ; pour `cairn_installation`, les
  opérations de portée `read` ; pour `cairn_workspace`, `open_project`, `list_nodes`,
  `exclusive_status`, `pending_questions` et la lecture des tâches déléguées
  (`task_status`). Les outils et commandes des plugins sont refusés, comme
  `delegate_task`, `task_cancel`, `wait_for`, `publish_artifact` ou `report_outcome`. Ces restrictions
  suivent l’autorisation du tour (`chatExecution.plan`), que le tour suivant remplace.

Les préréglages du moteur ne servent qu’à guider le modèle. Cairn découvre ceux de
Codex à chaque tour et envoie `plan`, puis `default` au tour qui suit un tour de plan.
Si Codex n’en propose pas, le tour reste borné et reçoit seulement la consigne de
Cairn.

Le passage à l’implémentation est explicite : un message ordinaire dans le même fil,
envoyé depuis le composeur ou le bouton **Implement** / **Implémenter** du dernier
plan proposé. Il s’exécute avec les droits courants de l’agent, contrôlés comme
avant par `covers` : il n’élargit aucun droit.

## Limites

- `readOnlyHint` est déclaré par le serveur MCP. Un serveur qui le donne à tort à un
  outil à effets peut agir pendant un tour de plan ; un outil sans annotation est
  refusé.
- Les commandes qui écrivent (compilation, tests) échouent pendant un tour de plan.
- Claude Code exige bubblewrap et socat pour son bac à sable. Une node qui ne les a
  pas refuse le tour de plan avec une erreur explicite, sans rien exécuter.
- Le refus du réseau s’applique au shell. `WebFetch` et la recherche web restent
  disponibles en lecture.
