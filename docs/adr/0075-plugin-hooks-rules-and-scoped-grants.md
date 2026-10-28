# Hooks de plugins, règles sans code et droits restreints par ressource

Date : 2026-10-28.

Cette décision remplace la délégation externe d’[ADR-0038](0038-luau-installation-plugins-and-external-bot-delegation.md), d’[ADR-0045](0045-declarative-plugin-contributions.md), d’[ADR-0046](0046-plugin-transports-and-forms-in-luau.md), d’[ADR-0057](0057-guided-plugin-setup.md), d’[ADR-0059](0059-delegated-tasks.md) et d’[ADR-0068](0068-owner-chosen-agents-for-bot-delegation.md) ; remplace les droits par instance d’[ADR-0045](0045-declarative-plugin-contributions.md) et d’[ADR-0051](0051-agent-centric-permissions.md) et les rôles d’[ADR-0029](0029-sharing-whole-installations.md) ; précise [ADR-0064](0064-opt-in-installation-tools-for-agents.md).

## Constat

Un plugin de bot comme Discord est aujourd’hui un front-end de conversation Cairn :
il admet les messages, démarre ou reprend une conversation pour leur auteur, puis
redessine dans Discord la progression, les questions, le bouton Stop et la réponse
(`plugins/discord/conversation.luau`). L’agent ne voit pas Discord : il ne peut ni
lire un fil, ni réagir, ni poster ailleurs. Ce mécanisme ne sert qu’au bot
conversationnel et ne se réutilise pas pour « quand X arrive, fais Y » (une pull
request ouverte, un e-mail reçu). L’instance choisit enfin l’agent qui répond et les
personnes autorisées, à l’inverse d’[ADR-0051](0051-agent-centric-permissions.md)
où l’agent porte ses droits ; un plugin est accordé à un agent en entier.

## Décision

Un plugin ne fait que déclarer ; le propriétaire et les membres décident du reste.

### Ce qu’un plugin déclare

- **Hooks** : les événements qu’il émet et leur payload (par exemple `message` avec
  auteur, salon et fil). Son transport émet ces hooks ; les capacités `inbound` et
  `conversations.submit` disparaissent. Cairn garde une file bornée par instance et
  déduplique les événements, comme pour l’admission qu’il remplace.
- **Outils et commandes** : leur schéma d’entrée, comme aujourd’hui.
- **Ressources** : ce sur quoi portent ses hooks et ses outils (serveurs, salons,
  fils, personnes, dépôts…), de quoi les lister, leur hiérarchie (un fil appartient à
  un salon, qui appartient à un serveur) et les entrées d’outil et champs de payload
  qui les désignent.
- **Modèles de règles** : des ensembles de règles prêts à l’emploi (« Bot
  conversationnel », « Support »). Installer un modèle demande ses paramètres
  (agent, salon…) puis crée des règles ordinaires appartenant à celui qui l’installe,
  sans lien avec le modèle : une mise à jour du plugin ne les modifie pas.

Les obligations du protocole d’un service (accuser réception d’un clic Discord en
moins de 3 secondes, heartbeats, reconnexions) restent au transport, qui répond
d’abord puis émet le hook ; les règles, asynchrones, modifient ensuite ce qu’il faut.

### Droits des agents

Le propriétaire accorde à un agent, pour une instance, soit l’accès complet, sans
limite et y compris ce qu’une version suivante du plugin ajoutera, soit ses
contributions une par une. Les outils, commandes et hooks accordés un par un peuvent
être restreints à des ressources : `discord.post` seulement dans #support, le hook
`message` seulement pour #support. Un droit sur une ressource couvre ses descendants
(un salon couvre ses fils). Cairn applique la restriction avant le plugin, à chaque
appel d’outil, et ne livre pas à une règle un événement dont la payload désigne une
autre ressource. Une restriction porte sur un type de ressource : chaque ressource de
ce type que désigne l’entrée ou la payload doit être couverte, les autres types
restant libres (restreindre le hook `message` à #support n’empêche pas son auteur
d’être n’importe qui). Si l’entrée ou la payload ne désigne aucune ressource
identifiable d’un type restreint, elle est refusée.
Les identifiants de run et les instructions ajoutées au prompt s’accordent aussi un
par un, sans restriction ; les conditions d’attente restent bornées par l’accès aux
projets. Rien n’est accordé par défaut. `cairn_installation` peut lire et réduire ces
droits, jamais les accorder. Un agent délégué
([ADR-0059](0059-delegated-tasks.md)) doit être couvert par son demandeur contribution
par contribution et ressource par ressource, pas seulement instance par instance.

### Accès des comptes aux agents

Un membre ne démarre, ne poursuit, n’oriente, n’arrête, ne lance à la main, ne
répond aux questions et n’utilise comme agent d’exécution d’une règle que les agents
auxquels il a accès ; il continue de voir
toutes les conversations ([ADR-0029](0029-sharing-whole-installations.md)). Ses
tâches planifiées sur un agent auquel il perd l’accès sont suspendues comme ses
règles. L’agent agit comme un rôle : y avoir accès, c’est pouvoir obtenir tout ce
qu’il peut faire. Un agent délégué n’exige pas cet accès : la couverture par son
demandeur borne déjà ses droits. Un agent qui a l’accès à l’installation la gère entière, avec
l’autorité de gestion du propriétaire
([ADR-0064](0064-opt-in-installation-tools-for-agents.md)) : avoir accès à cet agent
revient à avoir accès à tous les agents, et ce qu’un membre lui fait créer survit à
son départ.

Seul le propriétaire donne ou retire ces accès, et seulement depuis le web et
Android : comme la gestion des membres, ils sont exclus du MCP public et de
`cairn_installation`, pour qu’aucun agent ni client externe ne puisse le faire. Le
propriétaire a accès à tous les agents ; un nouvel agent n’est accessible qu’à lui. À
la mise en place, les membres gardent l’accès aux agents existants. Ces accès
appartiennent à une adhésion : réinviter un membre ne les rend pas.

Les personnes externes (un auteur Discord) n’ont ni identité ni droit dans Cairn :
elles ne sont qu’une donnée de la payload, que les conditions d’une règle examinent.

### Règles de hook

Une règle associe à un hook un arbre exécuté pour chaque événement. Le créateur
choisit ainsi, pour chaque hook, qui peut déclencher quoi : un bot ouvert à tous est
une règle qui ne teste pas l’auteur. Il n’y a pas de filtre séparé de l’arbre.

- **Agent d’exécution** : la règle désigne l’agent dont les droits encadrent le hook
  qu’elle écoute et les outils qu’elle appelle elle-même ; son créateur doit y avoir
  accès. Un membre ne peut donc pas écouter une instance que ses agents n’ont pas le
  droit d’écouter. Les droits sur les hooks et les outils n’existent que sur les
  agents.
- **Blocs** : si / sinon imbriqués et boucles.
- **Conditions** : un éditeur structuré de groupes ET / OU imbriqués, d’opérateurs
  sur tout champ de la payload (égal, contient, expression régulière, dans une liste,
  présent) et de prédicats de Cairn (l’auteur fait partie de personnes choisies,
  l’auteur a démarré la conversation d’une clé, une conversation existe pour une clé,
  quota par auteur).
- **Formules** : toute valeur d’une action ou d’une condition peut être une formule
  en [CEL](https://cel.dev) (Common Expression Language), complétée par des fonctions
  de Cairn (tronquer, découper en morceaux, formater une date…). Une formule est pure,
  sans effet de bord ni boucle, et son coût d’évaluation est borné.
- **Actions** : déclencher un agent, orienter une conversation, appeler un outil,
  annuler la conversation d’une clé, répondre à la question d’un agent, mémoriser une
  valeur, attendre puis reprendre. Chaque action peut utiliser la payload et le
  résultat des actions précédentes (`{{steps.post.messageId}}`) et ne fait qu’une
  chose : déclencher un agent avec une clé déjà liée échoue, et c’est l’arbre qui
  choisit d’orienter la conversation existante.
- **Clé de conversation** : déclencher un agent peut rattacher la conversation à une
  clé tirée de la payload (`discord:{{threadId}}`). Les clés appartiennent au créateur
  de la règle : deux membres qui utilisent la même clé désignent deux conversations
  différentes, et une règle n’atteint que les clés de son créateur. Orienter, annuler,
  répondre à une question ou mémoriser une valeur sur la conversation d’une clé exige
  en plus que le créateur ait accès à l’agent de cette conversation.
- **Valeurs mémorisées** : soit sur une conversation, par sa clé, partagées par les
  règles qui la touchent et supprimées avec elle, soit propres à la règle.
- **Hooks de Cairn** : Cairn émet quatre hooks pour les conversations rattachées à
  une clé : réponse prête (texte et livrables), question posée, progression (environ
  toutes les 8 secondes, une étape courte sans détail, une seule en attente par
  conversation) et fin (terminée, échouée ou annulée). Une règle ne les reçoit que
  pour les clés de son créateur, et tant qu’il a accès à l’agent de la conversation.
- **Conversations démarrées** : une conversation démarrée par une règle a son créateur
  pour auteur, qui ne la rend pas privée
  ([ADR-0029](0029-sharing-whole-installations.md)) ; elle note l’auteur externe
  comme origine.
- **Versions** : une exécution garde la version de la règle avec laquelle elle a
  commencé ; une modification ne vaut que pour les événements suivants.
- **Historique** : chacun voit l’état des exécutions des règles qu’il voit ; leur
  détail (payload et résultats des actions) n’est visible que du créateur, du
  propriétaire et des membres qui ont accès à l’agent d’exécution.
- **Échecs** : une erreur passagère (limite de débit, service indisponible) fait
  réessayer l’action en échec seule, avec un délai croissant qui respecte celui que
  demande le service ; les actions déjà faites ne sont pas rejouées. Une erreur
  définitive, ou des essais épuisés, arrête l’exécution. L’historique des exécutions
  est visible par ceux qui voient la règle.

Un bot Discord devient un agent qui a le hook `message` et les outils `read`, `post`
et `react` limités à un salon, accompagné des règles du modèle « Bot
conversationnel », dont les conditions choisissent les auteurs Discord auxquels il
répond. Le plugin garde son transport, ses hooks, ses outils et ses ressources ; le
rendu des conversations Cairn dans Discord disparaît.

### Autorité des règles

Une règle s’exécute avec l’autorité de son créateur, revérifiée à chaque action : un
droit manquant pour une action la fait échouer. Seul le créateur modifie sa règle.
Les autres membres la voient sans pouvoir la modifier ; ils peuvent la dupliquer
s’ils ont tous les droits qu’elle utilise, le duplicata leur appartenant et démarrant
désactivé pour ne pas répondre deux fois au même événement. Le propriétaire peut
supprimer n’importe quelle règle ou la mettre en pause en urgence, sans la modifier ;
la pause arrête aussi ses exécutions en cours, et seul le propriétaire la lève.
Lever cette pause et supprimer la règle d’un autre restent hors de
`cairn_installation` ; un agent n’y modifie ou ne supprime que les règles dont le
créateur est le compte pour lequel il travaille.

Quand le créateur perd l’accès à l’agent d’exécution, ou que cet agent perd le hook
de plugin écouté, la règle est suspendue, pas supprimée ; elle repart quand le droit revient.
Comme les tâches ([ADR-0029](0029-sharing-whole-installations.md)), une règle
appartient à une adhésion : retirer, faire partir ou supprimer le créateur la
désactive définitivement, et le réinviter ne la relance pas. Elle reste visible ; le
propriétaire peut la supprimer et un membre la dupliquer. Comme pour les tâches
planifiées, une exécution exige une adhésion vérifiée récemment ; si elle ne peut pas
l’être, l’exécution attend.

Un agent qui a l’accès à l’installation peut créer des règles. Leur créateur est le
compte pour lequel il travaille : celui qui a écrit le message que traite son tour,
l’auteur de la tâche pour une exécution planifiée (le propriétaire pour une mission
créée par un agent, [ADR-0064](0064-opt-in-installation-tools-for-agents.md)), ou le
créateur de la règle pour un tour qu’une règle a déclenché. Il ne peut accorder de
droits à aucun agent ni à aucun compte. Une règle créée par le MCP public a pour
créateur le propriétaire qui a autorisé ce client.

Une règle n’atteint un agent dont la politique autorise l’exécution native, ou une
conversation qui s’exécute en natif, que si son créateur détenait l’autorité native
([ADR-0050](0050-trusted-native-execution.md)) dans la requête authentifiée qui a
enregistré cette version de la règle. Une requête de `cairn_installation`, ou du MCP
public sans autorité native, ne peut pas l’accorder, comme pour les tâches déléguées
([ADR-0059](0059-delegated-tasks.md)). L’autorité est conservée avec la version et
revérifiée à chaque action qui résout, à l’exécution, un agent ou une conversation
natifs ; sans elle, l’action échoue. Le travail natif qu’une règle démarre garde les
limites de node et le renouvellement de bail de tout travail natif.

### Budget du moteur

Le moteur est borné pour qu’une boucle (une action qui déclenche le hook qui l’a
lancée) s’arrête d’elle-même. Cairn ne peut pas toujours relier un appel d’outil à
l’événement qu’il provoque en retour chez le service ; le budget est donc porté par
chaque règle : un plafond d’exécutions et un plafond de déclenchements d’agent par
heure, avec des valeurs par défaut que le créateur peut changer sous un maximum fixé
par Cairn. Au dépassement, la règle est suspendue jusqu’à ce que son créateur ou le
propriétaire la relance, et son créateur est prévenu ; cette suspension laisse finir
les exécutions en cours, que bornent leurs propres limites. Les chaînes que Cairn sait suivre,
quand une action provoque un hook de Cairn, ont en plus une profondeur maximale.
Chaque exécution est limitée en nombre de pas (blocs et itérations) et d’appels
d’outils, sans compter les attentes ; au dépassement, elle s’arrête en erreur.

Une exécution qui se termine sans effet (aucune action exécutée en dehors des blocs et
des conditions) ne compte pas dans les plafonds horaires et n’est pas gardée
dans l’historique, qui n’en garde que le nombre : un salon actif n’épuise pas le
budget de la règle. La progression, déjà cadencée par Cairn, ne compte pas non plus.

### Sans migration

Les bots et délégations existants sont supprimés plutôt que convertis : la seule
installation qui les utilise est celle du propriétaire du projet. Leur suppression
révoque leur travail en cours et ses descendants. Les droits existants d’un agent sur
une instance deviennent un accès complet à cette instance, qui comprend désormais ses
hooks : par choix du propriétaire, les membres peuvent donc écouter dès la mise en
place les instances que leurs agents utilisent, jusqu’à ce qu’il réduise ces droits.

## Options écartées

- **Garder la délégation par instance** : spécifique au bot conversationnel et
  contraire au modèle où l’agent porte ses droits.
- **Donner des droits à des identités externes** : inutile dès qu’une règle agit
  avec l’autorité de son créateur ; examiner l’auteur suffit.
- **Donner aux comptes des droits directs sur les outils** : un second système de
  permissions à côté de celui des agents.
- **Seulement des droits sur l’instance entière** : impossible alors de donner un
  salon sans donner tout le serveur.
- **Un langage de formules propre à Cairn, ou des expressions Luau** : le premier
  serait à écrire et maintenir, les secondes rapprochent les règles du code et
  alourdissent chaque évaluation. CEL est standard, borné et a une implémentation
  Rust (`cel-interpreter`).
- **Des filtres séparés de l’arbre** : écartés au profit d’un seul arbre par règle.
- **Une suite d’actions linéaire** : il aurait fallu doubler les règles avec des
  conditions opposées pour chaque alternative.
- **Seulement des hooks et des outils, sans règles** : chaque message déclencherait un
  agent, sans moyen de répondre de façon fiable.

## Conséquences

Les risques d’un droit trop large relèvent du propriétaire ; Cairn doit en revanche
rendre ces droits lisibles et n’en accorder aucun par défaut. Les capacités d’un
plugin qui ne passent pas par des entrées d’outil ne peuvent pas être restreintes :
un outil doit recevoir en entrée ce qu’il touche.

Un agent déclenché par une règle agit pour son créateur, à partir d’un contenu écrit
par des inconnus. S’il a l’accès à l’installation, un auteur externe peut tenter de
lui faire créer des règles au nom du créateur : un agent qui répond à un salon public
ne devrait pas avoir cet accès.

Comme toute capacité de l’interface, les règles (éditeur compris) et les droits par
ressource arrivent sur le web, Android, le MCP public et `cairn_installation` en même
temps, avec les limites ci-dessus : `cairn_installation` n’accorde aucun droit, et
l’accès des comptes aux agents reste au web et à Android.
