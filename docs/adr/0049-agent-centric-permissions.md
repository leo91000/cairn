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

L’agent porte toutes ses permissions. Deux champs s’ajoutent à `access` :

- `exclusiveNodes` : nodes sur lesquelles l’agent peut suspendre les autres
  conversations ;
- `plugins` : instances de plugins (dont les comptes 1Password) dont il reçoit les
  commandes, outils et instructions.

Aucun accès n’est accordé par défaut, Main compris. Le web et Android présentent une
page d’agent : un résumé des accès effectifs, une section par domaine, un brouillon
unique et une revue des changements sensibles avant l’enregistrement. Les ressources
gardent leurs réglages techniques et n’affichent plus les agents qu’en lecture seule.
Seule l’étape finale d’ajout d’une machine propose encore de choisir qui peut
l’utiliser, avec l’API existante `PUT /nodes/{id}/agents`.

Ces deux droits restent appliqués en direct, pas figés au moment de la mise en file :
`policy()` les exclut. Les retirer n’interrompt donc pas le travail en file et ne
suspend pas une délégation externe. Le catalogue MCP de gestion ne les expose pas :
un agent qui gère l’installation ne peut pas s’accorder un compte 1Password ni une
réservation exclusive, et ses modifications conservent ceux que le propriétaire a
réglés.

Chaque enregistrement change la `revision` de l’agent. Un éditeur qui renvoie une
révision périmée reçoit un conflit au lieu d’écraser des droits plus récents.
`POST /agents/{id}/preview` décrit, sans rien écrire, les exécutions qu’une réduction
d’accès arrêterait et les délégations (bot Discord) qui seraient suspendues. Le
propriétaire peut réapprouver ces délégations dans le même enregistrement
(`reapproveDelegations`), sous les mêmes règles que l’écran de l’instance.

## Compatibilité

Les nodes et les instances gardent une copie, `exclusiveAgents` et `agentIds`, écrite
dans la même transaction que l’agent. L’application des droits lit cette copie,
comme les versions précédentes. Une version antérieure lit donc les mêmes droits
après un retour arrière, et l’epoch de compatibilité ne change pas. Ces versions ne
modifient que la copie. Au démarrage, toute différence vient donc d’elles, et la
copie l’emporte : le côté agent est reconstruit, et les identifiants d’agents
supprimés sont retirés des copies. Les anciens clients qui envoient encore
`exclusiveAgents` à une node ou `agentIds` à une instance mettent à jour les agents,
et leur omission conserve les droits existants.

La version précédente fusionne toutes les clés de `access` dans sa politique, et
compare cette politique à l’accès approuvé d’une délégation externe. Une liste vide
est donc enregistrée comme une clé absente, et l’accès approuvé (`grantedAccess`)
inclut les listes non vides. Cette version compare les approbations sans les droits
en direct ; elle met à jour l’accès approuvé d’une délégation inchangée quand ces
droits changent. Après un retour arrière, la version précédente calcule ainsi le même
accès approuvé et ne suspend pas les délégations. Le démarrage donne aussi une
révision aux agents enregistrés avant son introduction.

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
- Les comptes d’agent de code restent un pool partagé (ADR 0002) : la page l’indique
  sans créer de droit.
