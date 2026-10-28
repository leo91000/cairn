> Statut : acceptée. Remplace la délégation externe d’[ADR-0038](0038-luau-installation-plugins-and-external-bot-delegation.md) et d’[ADR-0068](0068-owner-chosen-agents-for-bot-delegation.md), et étend les droits par instance d’[ADR-0051](0051-agent-centric-permissions.md).

# Hooks de plugins, règles sans code et droits restreints par ressource

Date : 2026-10-28.

## Constat

Un plugin de bot comme Discord est aujourd’hui un front-end de conversation Cairn :
il admet les messages, démarre ou reprend une conversation pour leur auteur, puis
redessine dans Discord la progression, les questions, le bouton Stop et la réponse
(`plugins/discord/conversation.luau`). L’agent ne voit pas Discord : il ne peut ni
lire un fil, ni réagir, ni poster ailleurs. Ce mécanisme ne sert qu’au bot
conversationnel et ne se réutilise pas pour « quand X arrive, fais Y » (une pull
request ouverte, un e-mail reçu). L’instance choisit enfin l’agent qui répond et les
personnes autorisées, à l’inverse d’ADR-0051 où l’agent porte ses droits ; un plugin
est accordé à un agent en entier, avec tous ses outils.

## Décision

Un plugin ne fait que déclarer ; le propriétaire décide de tout le reste.

- **Hooks** : le plugin déclare les événements qu’il émet et leur payload (par
  exemple `message` avec auteur, canal et fil). Cairn émet aussi quatre hooks pour
  ses conversations : réponse prête (texte et livrables), question posée,
  progression (environ toutes les 8 secondes, une étape courte sans détail) et fin
  (terminée, échouée ou annulée). La progression, déjà cadencée par Cairn, ne compte
  pas dans le plafond d’exécutions d’une règle.
- **Outils** : le plugin déclare ses outils et leur schéma d’entrée, comme
  aujourd’hui.
- **Ressources** : le plugin fournit de quoi lister ce sur quoi portent ses hooks et
  ses outils (serveurs, salons, personnes, dépôts…) et indique quelles entrées
  d’outil et quels champs de payload désignent ces ressources.
- **Droits par agent** : le propriétaire accorde à un agent soit l’accès complet à
  une instance, sans limite et y compris les outils qu’une version suivante du
  plugin ajoutera, soit des outils et commandes un par un, restreints à des
  ressources choisies (par exemple `discord.post` seulement dans #support). Cairn
  applique la restriction à chaque appel, avant le plugin. Ce qui ne passe pas par
  une entrée (identifiants de run, instructions ajoutées au prompt, conditions
  d’attente) reste un droit sur l’instance entière. Rien n’est accordé par défaut.
- **Accès des comptes aux agents** : le propriétaire donne ou retire à des comptes
  Cairn l’accès à un agent. L’agent agit comme un rôle : y avoir accès, c’est pouvoir
  obtenir tout ce qu’il peut faire. Les personnes externes (un auteur Discord)
  n’ont aucune identité ni aucun droit dans Cairn : elles ne sont qu’une donnée de la
  payload, que les conditions d’une règle examinent. La conversation qu’une règle
  démarre appartient à son créateur et note l’auteur externe comme origine.
- **Règles de hook** : une interface sans code associe à un hook un arbre d’actions,
  exécuté pour chaque événement. Le créateur choisit ainsi explicitement, pour chaque
  hook, qui peut déclencher quoi : un bot ouvert à tous est une règle qui ne teste
  pas l’auteur. Il n’y a pas de filtre séparé de l’arbre. Cairn déduplique les
  événements de lui-même. Une exécution qui se termine sans effet (aucun outil
  appelé, aucun agent déclenché ou orienté, rien mémorisé) ne compte pas dans les
  plafonds horaires et n’est pas gardée dans l’historique, qui n’en garde que le
  nombre : un salon actif n’épuise pas le budget de la règle.
- **Versions** : une exécution garde la version de la règle avec laquelle elle a
  commencé ; une modification ne vaut que pour les événements suivants. Les droits
  sont revérifiés à chaque action.
- **Conditions** : un éditeur structuré de groupes ET / OU imbriqués, d’opérateurs
  sur tout champ de la payload (égal, contient, expression régulière, dans une liste,
  présent) et de prédicats de Cairn (l’auteur fait partie de personnes choisies,
  l’auteur a démarré la conversation d’une clé, une conversation existe pour une clé,
  quota par auteur).
- **Formules** : toute valeur d’une action ou d’une condition peut être une formule
  en [CEL](https://cel.dev) (Common Expression Language), complétée par des fonctions
  de Cairn (tronquer, découper en morceaux, formater une date…). Une formule est pure,
  sans effet de bord ni boucle, et son coût d’évaluation est borné ; les boucles sont
  des blocs de l’arbre.
- **Actions** : déclencher un agent, orienter une conversation, appeler un outil,
  annuler la conversation d’une clé, répondre à la question d’un agent, mémoriser une
  valeur par clé, attendre puis reprendre. Chaque action peut utiliser la payload et
  le résultat des actions précédentes (`{{steps.post.messageId}}`). Chaque action ne
  fait qu’une chose : déclencher un agent avec une clé déjà liée échoue, et c’est la
  règle qui choisit d’orienter la conversation existante.
- **Blocs de contrôle** : une règle est un arbre qui dispose des blocs standard, si /
  sinon imbriqués et boucles, dont les conditions utilisent l’éditeur ci-dessus.
- **Événements hors des règles** : les obligations du protocole d’un service
  (accuser réception d’un clic Discord en moins de 3 secondes, heartbeats,
  reconnexions) restent au transport du plugin, qui répond d’abord puis émet le hook.
  Les règles, asynchrones, modifient ensuite ce qu’il faut.
- **Agent d’exécution** : une règle désigne l’agent dont les droits encadrent ses
  appels d’outils directs ; son créateur doit avoir accès à cet agent. Les droits sur
  les outils n’existent que sur les agents.
- **Valeurs mémorisées** : une règle mémorise des valeurs soit sur une conversation,
  par sa clé, partagées par toutes les règles qui la touchent et supprimées avec
  elle, soit pour elle seule.
- **Clé de conversation** : déclencher un agent peut associer la conversation à une
  clé tirée de la payload (`discord:{{threadId}}`). Orienter vise la conversation
  d’une clé, et les hooks de cette conversation la portent, ce qui permet de répondre
  au bon endroit.
- **Modèles de règles** : un plugin peut fournir des ensembles de règles prêts à
  l’emploi (« Bot conversationnel », « Support »). Installer un modèle demande ses
  paramètres (agent, salon…) puis crée des règles ordinaires appartenant à celui qui
  l’installe, sans lien avec le modèle : une mise à jour du plugin ne les modifie pas.
- **Échecs** : une erreur passagère (limite de débit, service indisponible) fait
  réessayer l’action en échec seule, avec un délai croissant qui respecte celui que
  demande le service ; les actions déjà faites ne sont pas rejouées. Une erreur
  définitive, ou des essais épuisés, arrête l’exécution. L’historique des exécutions
  est visible par ceux qui voient la règle.

Un bot Discord devient un agent qui a les outils `read`, `post` et `react` limités à
un salon, et les règles du modèle « Bot conversationnel », dont les conditions
choisissent les auteurs Discord auxquels il répond. Le plugin garde son transport, ses hooks, ses outils et ses
ressources ; le rendu des conversations Cairn dans Discord disparaît.

## Options écartées

- **Garder la délégation par instance** : spécifique au bot conversationnel et
  contraire au modèle où l’agent porte ses droits.
- **Donner des droits à des identités externes** : inutile dès qu’une règle agit
  avec l’autorité de son créateur ; filtrer l’auteur suffit.
- **Donner aux comptes des droits directs sur les outils** : un second système de
  permissions à côté de celui des agents.
- **Un langage de formules propre à Cairn, ou des expressions Luau** : le premier
  serait à écrire et maintenir, les secondes rapprochent les règles du code et
  alourdissent chaque évaluation. CEL est standard, borné et a une implémentation
  Rust (`cel-interpreter`).
- **Des filtres séparés de l’arbre** : écartés au profit d’un seul arbre par règle.
- **Une suite d’actions linéaire** : il aurait fallu doubler les règles avec des
  filtres opposés pour chaque alternative.
- **Seulement des hooks et des outils, sans règles** : chaque message déclencherait un
  agent, sans filtrage préalable ni moyen de répondre de façon fiable.
- **Accorder un plugin entier à un agent** : impossible alors de donner un salon sans
  donner tout le serveur.

## Autorité des règles

Une règle de hook s’exécute avec l’autorité de son créateur, revérifiée à chaque
exécution : s’il n’a pas accès à ce qu’une action utilise, l’action échoue et c’est à
lui de régler ses droits ou sa règle. Seul le créateur modifie sa règle. Les autres
membres la voient sans pouvoir la modifier ; ils peuvent la dupliquer s’ils ont tous
les droits qu’elle utilise, le duplicata leur appartenant et démarrant désactivé pour
ne pas répondre deux fois au même événement. Le propriétaire de l’installation peut
supprimer n’importe quelle règle ou la mettre en pause en urgence, sans la modifier.

Quand le créateur perd ses droits ou quitte l’installation, ses règles sont
suspendues, pas supprimées : elles restent visibles, le propriétaire peut les
supprimer et un membre les dupliquer. Elles repartent si le créateur retrouve ses
droits.

Le moteur d’exécution est borné pour qu’une boucle (une action qui déclenche le hook
qui l’a lancée) s’arrête d’elle-même. Cairn ne peut pas toujours relier un appel
d’outil à l’événement qu’il provoque en retour chez le service ; le budget est donc
porté par chaque règle : un plafond d’exécutions et un plafond de déclenchements
d’agent par heure, avec des valeurs par défaut que le créateur peut changer. Au
dépassement, la règle se met en pause et son créateur est prévenu. Les chaînes que
Cairn sait suivre, quand une action provoque un hook de Cairn, ont en plus une
profondeur maximale. Chaque exécution est en outre limitée en nombre de pas (blocs et
itérations) et d’appels d’outils, sans compter les attentes ; au dépassement, elle
s’arrête en erreur. Ces limites ont aussi des valeurs par défaut que le créateur peut
changer.

## Sans migration

Les bots et délégations existants sont supprimés plutôt que convertis : la seule
installation qui les utilise est celle du propriétaire du projet. Les droits
existants d’un agent sur une instance (ADR-0051) deviennent un accès complet à cette
instance : rien ne change pour les agents en place.

## Conséquences

Les risques d’un droit trop large relèvent du propriétaire ; Cairn doit en revanche
rendre ces droits lisibles et n’en accorder aucun par défaut. Les capacités d’un
plugin qui ne passent pas par des entrées d’outil ne peuvent pas être restreintes :
un outil doit recevoir en entrée ce qu’il touche. Comme toute capacité de l’interface,
les règles (éditeur compris), les droits par ressource et l’accès des comptes aux
agents arrivent sur le web, Android, le MCP public et `cairn_installation` en même
temps. Un agent qui a accès à l’installation peut créer des règles, comme des
missions ([ADR-0064](0064-opt-in-installation-tools-for-agents.md)) : leur créateur
est le compte pour lequel il travaille, et il ne peut accorder de droits ni à
lui-même ni à l’agent d’exécution choisi.
