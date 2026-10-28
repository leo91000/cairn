> Statut : étendue par [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) : un droit sur une instance de plugin devient un accès complet, à côté de droits contribution par contribution, dont les outils, commandes et hooks peuvent être restreints à des ressources ; le MCP public expose ces droits, `cairn_installation` peut les lire et les réduire sans jamais les élargir ; les délégations externes disparaissent.

# Toutes les permissions d’un agent se règlent sur sa page

Date : 2026-10-18.

## Constat

Les droits d’un agent étaient dispersés. Projets, skills, connexions MCP, GitHub,
nodes et gestion de l’installation se réglaient dans l’éditeur de l’agent. La
réservation exclusive d’une node se cochait dans la configuration de cette node,
sous ses budgets (`node.exclusiveAgents`). L’accès aux instances de plugins, dont les
comptes de service 1Password, se cochait sur chaque instance (`agentIds`). L’accès
aux nodes pouvait aussi se modifier depuis la node. On ne voyait donc nulle part
« à quoi cet agent a accès et ce qu’il peut faire », et certains droits restaient
introuvables.

## Décision

L’agent porte toutes ses permissions. Deux champs s’ajoutent à l’agent, à côté de
`access` :

- `exclusiveNodes` : nodes sur lesquelles l’agent peut suspendre les autres
  conversations ;
- `plugins` : instances de plugins (dont les comptes 1Password) dont il reçoit les
  commandes, outils et instructions.

Aucun accès n’est accordé par défaut, Main compris. Le web et Android présentent une
page d’agent : un résumé des accès effectifs, une section par domaine, un brouillon
unique et une revue des changements sensibles avant l’enregistrement. Les ressources
gardent leurs réglages techniques et n’affichent plus les agents qu’en lecture seule.
Seule l’étape finale d’ajout d’une machine propose encore de choisir qui peut
l’utiliser, avec l’API existante `PUT /nodes/{id}/agents` en mode `add`, qui accorde
sans jamais retirer. Une réservation exclusive exige le droit d’exécuter sur la node :
le retirer retire aussi la réservation. Elle ne concerne que les VM Firecracker : un
agent natif, ou une node native, ne peut pas en recevoir.

Ces deux droits restent appliqués en direct, pas figés au moment de la mise en file :
ils ne font pas partie de `access`, donc de `policy()`. Les retirer n’interrompt donc pas le travail en file et ne
suspend pas une délégation externe ; accorder un compte 1Password (ou une autre
instance de plugin) à l’agent d’une délégation ouverte à tout le monde la suspend,
et l’aperçu l’indique. Une réservation exclusive ne la suspend pas, comme avant. Le catalogue MCP de
gestion ne les expose pas :
un agent qui gère l’installation ne peut pas s’accorder un compte 1Password ni une
réservation exclusive, et ses modifications conservent ceux que le propriétaire a
réglés, sauf les réservations qu’il rend impossibles : celle d’une node sur laquelle
il retire le droit d’exécuter, ou toutes s’il passe l’agent en exécution native.

Chaque enregistrement change la `revision` de l’agent. Un éditeur qui renvoie une
révision périmée reçoit un conflit au lieu d’écraser des droits plus récents.
`POST /agents/{id}/preview` décrit, sans rien écrire, les exécutions qu’une réduction
d’accès arrêterait et les délégations (bot Discord) qui seraient suspendues. Le
propriétaire peut réapprouver ces délégations dans le même enregistrement
(`reapproveDelegations`), sous les mêmes règles que l’écran de l’instance. Seules
celles que cette modification suspend sont proposées cochées : une délégation déjà
suspendue par une modification antérieure (`alreadySuspended`) reste à réapprouver
explicitement, pour ne jamais approuver des accès que le propriétaire n’a pas revus.

## Compatibilité

Les nodes et les instances gardent une copie, `exclusiveAgents` et `agentIds`, écrite
dans la même transaction que l’agent. L’application des droits lit cette copie,
Au démarrage, la copie l’emporte : le côté agent est reconstruit et les identifiants
d’agents supprimés sont retirés des copies. Les anciens clients qui envoient encore
`exclusiveAgents` à une node ou `agentIds` à une instance mettent à jour les agents,
et leur omission conserve les droits existants.

La version précédente fusionne toutes les clés de `access` dans sa politique, et
compare cette politique à l’accès approuvé d’une délégation externe. Les deux droits
sont donc enregistrés hors de `access` : les deux versions calculent la même
politique, y compris après un enregistrement par la version précédente, qui ignore
ces champs (les copies sur les ressources restent, et le démarrage suivant reconstruit
l’agent). Le démarrage donne aussi une révision aux agents enregistrés avant son
introduction.

Depuis la campagne de refacto (2026-10-25), les clients exigent le protocole et les
API courants. Le marqueur `grantsOnAgent`, les droits en lecture seule pour les
anciennes installations et le repli sans aperçu ont été supprimés. Les instances
éditées omettent `agentIds` : le serveur conserve leurs droits en direct.

Les autres modifications d’un agent conservent ses droits en direct, par exemple
la suppression d’une connexion MCP, qui change aussi sa révision. Un droit déjà
détenu sur une instance dont le package attend une revue reste accepté, pour que
les autres modifications de l’agent s’enregistrent ; l’accorder à un autre agent
exige un package actif.

La suppression d’un agent le retire désormais aussi de `exclusiveAgents`. Avant,
cet identifiant orphelin faisait échouer l’enregistrement suivant de la node.

## Conséquences

- 1Password reste autorisé par compte de service : le propriétaire règle dans
  1Password ce que voit le compte, et crée un autre compte pour d’autres droits.
- Les délégations externes restent choisies sur l’instance (quel agent répond aux
  personnes). La page de l’agent les montre et propose leur réapprobation.
- Les comptes d’agent de code restent un pool partagé ([ADR-0061](0061-one-account-pool-for-coding-agents.md)) : la page l’indique
  sans créer de droit.
