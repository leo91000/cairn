# Les programmations sont des règles de hook déclenchées par l’Horloge

Date : 2026-11-08.

Statut : acceptée, non implémentée. Elle précise
[ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) et
[ADR-0074](0074-common-agent-question-tool.md), et remplacera, une fois livrée, les
missions et leurs runs.

## Constat

Cairn a deux façons de faire travailler un agent : la conversation et la mission. Les
deux reposent déjà sur la table `runs` : le run d’une conversation porte une
pseudo-tâche construite à partir des réglages d’une mission. Une mission ajoute un
brief, un cron et une exécution sans surveillance : le run ne pose pas de question et
ne reçoit pas de suite, sauf à le convertir en conversation (`convert_run_to_chat`).

Cette dualité se paie dans tout le code. Les attentes durables existent en deux
variantes ([MISSION-WAITS](../MISSION-WAITS.md),
[ADR-0065](0065-durable-conversation-wakeups.md)). L’autorité native, la migration
de mode d’exécution, les déplacements de node et la reprise traitent chacun les deux
cas (environ 25 tests de `trigger` contre `"chat"` dans `crates/installation`). Web, Android, le MCP public et
`cairn_installation` ont chacun leurs écrans et outils de missions. Une partie de ce
que les missions portaient a d’ailleurs déjà migré vers les conversations : la
délégation passe par des tâches déléguées
([ADR-0059](0059-delegated-tasks.md)), et les règles de hook démarrent des
conversations, jamais des missions
([ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md)).

Il reste deux besoins propres aux missions : faire travailler un agent à heure fixe,
et l’administrer comme un objet durable, avec son auteur, son historique et sa pause.
ADR-0075 donne déjà aux règles de hook un créateur dont l’autorité est revérifiée à
chaque action, des empêchements (désactivée, pause d’urgence, budget épuisé, droits
perdus, adhésion terminée), un historique des déclenchements, des versions, la duplication
et un éditeur sans code. Seul un déclencheur horaire manque.

## Décision

**La conversation est la seule unité de travail d’un agent.** La mission disparaît.

### L’Horloge et les programmations

- Cairn émet un hook **Horloge**, paramétré par la règle qui l’écoute : un cron à
  cinq champs ou une date unique, dans un fuseau IANA. Sa payload porte la date
  prévue et la date d’émission. Elle s’ajoute aux hooks de Cairn d’ADR-0075, mais
  n’est liée à aucune clé ni conversation et n’exige aucun droit d’agent : la règle
  désigne tout de même un agent d’exécution, que son créateur doit pouvoir utiliser.
- Une **programmation** est une règle de hook ordinaire qui écoute l’Horloge et
  démarre un agent avec un brief. Ce n’est pas une entité à part : elle a le créateur,
  l’autorité, les états, l’historique, les versions, la duplication et le budget de
  toute règle. L’UI l’appelle « Programmation ».
- Chaque déclenchement ouvre **une nouvelle conversation**, sans session ni espace de
  travail partagé avec la précédente. Il n’y a pas de mode « même fil ».
- Cette conversation est **ordinaire** : l’agent peut poser des questions, et une
  personne peut lui répondre, l’orienter ou la poursuivre. Elle note la programmation
  comme origine.
- Une mission ponctuelle n’a plus d’équivalent : un lancement immédiat est un chat,
  et un lancement différé est une programmation à date unique.
- **Rattrapage** : après une interruption, les créneaux manqués produisent un seul
  déclenchement, avec la date prévue dans sa payload ; le créneau suivant est calculé
  à partir de maintenant.
- **Chevauchement** : un nouveau prédicat de Cairn, « une conversation démarrée par
  cette règle travaille », est vrai tant qu’un tour est en file, en cours ou en
  attente d’un réveil. Une question sans réponse ne compte pas, pour qu’un oubli ne
  bloque pas la programmation indéfiniment. L’éditeur rapide l’utilise pour sauter le
  déclenchement ; l’éditeur complet permet de l’enlever.
- **Éditeur rapide** : « Programmer un agent » demande l’agent, le projet, le brief,
  le cron ou la date, et le fuseau, puis crée une règle ordinaire, comme un modèle de
  règles fourni par Cairn, dont l’agent d’exécution est l’agent démarré. On la modifie
  ensuite dans l’éditeur complet.
  « Lancer maintenant » la déclenche sans attendre l’Horloge.

### Ce qui disparaît avec les missions

- `worktree=false` : une conversation travaille toujours sur une copie isolée du
  projet.
- La sélection explicite des skills : un agent reçoit toujours toutes ses skills.
- Les tags.
- `run_task` et les missions lancées par des missions : un agent démarre du travail
  immédiat par la délégation ([ADR-0059](0059-delegated-tasks.md)), avec ses limites,
  ou, s’il a l’accès à l’installation, par les outils de conversation de
  `cairn_installation`, comme aujourd’hui.
- Le « demandeur d’une mission » et la conversion d’un run en conversation.

Un agent qui a l’accès à l’installation gère les programmations comme toute règle :
seulement au nom d’une personne, dans une conversation qui remplit les conditions de
provenance d’ADR-0075, jamais depuis une conversation programmée, déléguée ou
démarrée par une règle.

### Résultats et « Pour vous »

- `report_outcome` garde `completed` et `blocked`. `needs_input` disparaît : l’agent
  pose une question à la place. Comme le prévoit ADR-0074 pour `needs_input`, un
  résultat `blocked` conserve les questionnaires en attente ; `completed` les retire.
- Le résultat `blocked` fait remonter n’importe quelle conversation dans « Pour vous ».
- Une conversation démarrée par une règle (programmation comprise) n’y apparaît que
  lorsqu’elle a besoin de quelqu’un : une question, un échec ou un résultat `blocked`.
  Un tour non lu ne suffit pas : une programmation quotidienne ajouterait sinon une
  ligne par jour. Dès qu’une personne y écrit, elle suit les règles des autres
  conversations.
- La mise de côté devient « reporter jusqu’à » et s’applique à toute conversation ;
  « écarter » devient « marquer lu ».

### Migration

Tout est migré ; il ne reste qu’un lecteur de conversations.

- Une mission devient une règle sur l’Horloge, dont le créateur est l’auteur de la
  mission. Une mission dont l’auteur a été retiré devient une règle « Adhésion
  terminée », qu’il faut dupliquer ; une mission suspendue faute d’accès de son auteur
  à l’agent devient une règle « Droits perdus ». Une mission désactivée ou archivée
  devient une règle désactivée.
- L’autorité native enregistrée sur la mission devient celle de la version migrée.
  Une mission sans autorité native enregistrée n’atteint pas d’agent natif tant que
  son créateur ne réenregistre pas la règle.
- Chaque run devient une conversation dont l’origine est cette règle, avec sa
  timeline, ses livrables et son résultat. Les missions qui utilisaient
  `worktree=false` ou une liste de skills explicite les perdent.

### Ordre de livraison

Les missions sont gelées : rien de nouveau ne s’y ajoute. L’Horloge, l’éditeur rapide,
la migration et la suppression des missions (web, Android, MCP public,
`cairn_installation`) s’ajoutent à la carte d’ADR-0075 et viennent après le modèle,
le moteur et l’interface des règles.

## Options écartées

- **Une entité « Programmation » à part**, livrable sans attendre le moteur de règles :
  elle aurait dupliqué l’autorité du créateur, l’historique, les versions et la pause
  que les règles fournissent déjà, et il aurait fallu la convertir plus tard.
- **Continuer le même fil à chaque déclenchement** : le contexte grossit sans fin, un
  échec met le fil en pause et l’historique déclenchement par déclenchement se perd.
- **Garder des missions sans surveillance** : c’est la cause des conversions de run en
  conversation et de `needs_input`, qui fait doublon avec une question.
- **Lancer les conversations en parallèle quand la précédente travaille encore** :
  une programmation fréquente accumulerait du travail concurrent sur le même sujet.

## Conséquences

Les programmations dépendent du moteur de règles : tant qu’il n’est pas livré, les
missions restent la seule façon de programmer un agent. Une fois les missions
remplacées, une conversation qu’une règle a démarrée, programmation comprise, ne crée
pas de programmation. Les entrées du glossaire qui dépendent des missions ou de la
sélection des skills seront retirées ou reformulées avec leur suppression, notamment
« Demandeur d’une mission », « Mise de côté d’un résultat », « Auteur d’une tâche »,
« Limite de stockage par tâche », « Migration du mode d’exécution d’une conversation »
et « Skill ».
