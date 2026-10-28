# Hooks de plugins, règles sans code et droits restreints par ressource

Date : 2026-10-28.

Cette décision remplace la délégation externe
d’[ADR-0038](0038-luau-installation-plugins-and-external-bot-delegation.md),
d’[ADR-0045](0045-declarative-plugin-contributions.md),
d’[ADR-0046](0046-plugin-transports-and-forms-in-luau.md),
d’[ADR-0051](0051-agent-centric-permissions.md),
d’[ADR-0057](0057-guided-plugin-setup.md), d’[ADR-0059](0059-delegated-tasks.md) et
d’[ADR-0068](0068-owner-chosen-agents-for-bot-delegation.md) ; remplace en partie les
droits par instance d’[ADR-0045](0045-declarative-plugin-contributions.md),
d’[ADR-0051](0051-agent-centric-permissions.md) et
d’[ADR-0056](0056-plugin-oauth-file-transfers-and-gmail.md), la couverture des tâches
déléguées d’[ADR-0059](0059-delegated-tasks.md), la règle d’une instance par package
d’[ADR-0053](0053-github-as-a-bundled-plugin.md), la réutilisation des étapes
partagées d’[ADR-0057](0057-guided-plugin-setup.md) et le rôle de membre
d’[ADR-0029](0029-sharing-whole-installations.md) ; précise
[ADR-0050](0050-trusted-native-execution.md) et
[ADR-0064](0064-opt-in-installation-tools-for-agents.md).

## Constat

Un plugin de bot comme Discord est aujourd’hui un front-end de conversation Cairn : il
admet les messages, démarre ou reprend une conversation pour leur auteur, puis
redessine dans Discord la progression, les questions, le bouton Stop et la réponse
(`plugins/discord/conversation.luau`). L’agent ne voit pas Discord : il ne peut ni
lire un fil, ni réagir, ni poster ailleurs. Ce mécanisme ne sert qu’au bot
conversationnel et ne se réutilise pas pour « quand X arrive, fais Y » (une pull
request ouverte, un e-mail reçu). L’instance choisit enfin l’agent qui répond et les
personnes autorisées, à l’inverse d’[ADR-0051](0051-agent-centric-permissions.md) où
l’agent porte ses droits ; un plugin est accordé à un agent en entier.

## Décision

Un plugin ne fait que déclarer ; le propriétaire et les membres décident du reste.

### Ce qu’un plugin déclare

- **Hooks** : les événements qu’il émet et leur payload (par exemple `message` avec
  auteur, salon et fil). Son transport émet ces hooks ; les capacités `inbound`,
  `conversations.submit` et `conversations.control` disparaissent : les règles
  répondent aux questions et arrêtent les conversations. Cairn garde une file bornée
  par instance et déduplique les événements, comme pour l’admission qu’il remplace.
- **Outils et commandes** : leur schéma d’entrée, comme aujourd’hui.
- **Ressources** : ce sur quoi portent ses hooks et ses outils (serveurs, salons,
  fils, personnes, dépôts…), de quoi les lister, leur hiérarchie (un fil appartient à
  un salon, qui appartient à un serveur) et les entrées d’outil, arguments de commande
  et champs de payload qui les désignent.
- **Modèles de règles** : des ensembles de règles prêts à l’emploi (« Bot
  conversationnel », « Support »). Installer un modèle demande ses paramètres (agent,
  salon…) puis crée des règles ordinaires appartenant à celui qui l’installe, sans
  lien avec le modèle : une mise à jour du plugin ne les modifie pas.

Les obligations du protocole d’un service (accuser réception d’un clic Discord en
moins de 3 secondes, heartbeats, reconnexions) restent au transport, qui répond
d’abord puis émet le hook ; les règles, asynchrones, modifient ensuite ce qu’il faut.

### Droits des agents

Le propriétaire accorde à un agent, pour une instance, soit l’accès complet, sans
limite et y compris ce qu’une version suivante du plugin ajoutera, soit ses
contributions une par une. Pour un package qui déclare des identifiants de run,
l’agent les reçoit d’une seule instance, que le propriétaire choisit explicitement et
que Cairn retient ; un accès complet aux autres instances ne les comprend pas. Retirer
le droit sur cette instance laisse l’agent sans identifiants de run pour ce package
jusqu’à un nouveau choix du propriétaire : une réduction n’élargit jamais un autre
droit. Les outils, commandes et hooks accordés un par un peuvent être restreints à des
ressources : `discord.post` seulement dans #support, le hook `message` seulement pour
#support. Un droit sur une ressource couvre ses descendants (un salon couvre ses
fils). Cairn applique la restriction avant le plugin, à chaque appel d’outil ou de
commande, et ne livre pas à une règle un événement dont la payload désigne une autre
ressource. Une restriction porte sur un type de ressource et sur ses descendants :
chaque ressource de ce type, ou d’un type descendant, que désigne l’entrée ou la
payload doit se trouver sous une ressource couverte, d’après la hiérarchie que Cairn
résout lui-même (un fil de #general est refusé quand seul
#support est accordé). Les autres types restent libres : restreindre le hook `message`
à #support n’empêche pas son auteur d’être n’importe qui. Une entrée ou une payload
qui ne désigne aucune ressource identifiable d’un type restreint, ou des ressources
contradictoires, est refusée.

Les identifiants de run, les instructions ajoutées au prompt et les conditions
d’attente s’accordent aussi un par un, sans restriction de ressource ; les conditions
restent bornées par l’accès aux projets, et les sources de projets et de skills
s’accordent de même. Des identifiants de run permettent à l’agent, chez le service,
tout ce que permet le compte de l’instance : les accorder rend vaines les restrictions
de ressource de ses outils. Un agent reçoit ses identifiants de run d’au plus une
instance par package, qu’il les reçoive seuls ou par un accès complet
([ADR-0053](0053-github-as-a-bundled-plugin.md)) ; ses autres contributions peuvent
venir de plusieurs instances, et seule une instance accordée en accès complet compte
comme accordée pour réutiliser ses étapes partagées de configuration
([ADR-0057](0057-guided-plugin-setup.md)). Rien n’est accordé par défaut. Accorder une
instance à la dernière étape de l’assistant de configuration donne l’accès complet.
`cairn_installation` peut lire et réduire ces droits, jamais les accorder. Un agent
délégué ([ADR-0059](0059-delegated-tasks.md)) doit être couvert par son demandeur
contribution par contribution et ressource par ressource, pas seulement instance par
instance ; la couverture est revérifiée à chaque action de la tâche, pas seulement à
sa création. Une fois la conversation demandeuse supprimée, la tâche déléguée exige
l’accès à son propre agent.

### Accès des comptes aux agents

Un membre ne démarre, ne poursuit, n’oriente, ne lance à la main, ne répond aux
questions et n’utilise comme agent d’exécution d’une règle que les agents auxquels il
a accès, ni ne crée ou ne modifie de tâche sur un autre agent ; arrêter un travail
reste permis à tous ; il continue de voir toutes les conversations
([ADR-0029](0029-sharing-whole-installations.md)). Ses tâches planifiées sur un agent
auquel il perd l’accès sont suspendues comme ses règles. L’agent agit comme un rôle :
y avoir accès, c’est pouvoir obtenir tout ce qu’il peut faire. Pour une tâche
déléguée, l’accès à l’agent de sa conversation demandeuse suffit à la poursuivre ou à
répondre à ses questions : la couverture par ce demandeur borne ses droits. Un agent
qui a l’accès à l’installation la gère entière, avec l’autorité de gestion du
propriétaire ([ADR-0064](0064-opt-in-installation-tools-for-agents.md)) : avoir accès
à cet agent revient, par ce qu’il peut faire, à avoir accès à tous les agents (les
vérifications d’accès restent celles des accès accordés), et ce qu’un membre lui fait
créer, hors ses règles, survit à son départ (une mission a le propriétaire pour
auteur).

Seul le propriétaire donne ou retire ces accès, et seulement depuis le web et
Android : comme la gestion des membres, ils sont exclus du MCP public et de
`cairn_installation`, pour qu’aucun agent ni client externe ne puisse le faire. Le
propriétaire a accès à tous les agents ; un nouvel agent n’est accessible qu’à lui. À
la mise en place, les membres gardent l’accès aux agents existants. Ces accès
appartiennent à une adhésion : réinviter un membre ne les rend pas.

Les personnes externes (un auteur Discord) n’ont ni identité ni droit dans Cairn :
elles ne sont qu’une donnée de la payload, que les conditions d’une règle examinent.

### Règles de hook

Une règle associe à un hook un arbre exécuté pour chaque événement. Le créateur
choisit ainsi, pour chaque hook, qui peut déclencher quoi : un bot ouvert à tous est
une règle qui ne teste pas l’auteur. Il n’y a pas de filtre séparé de l’arbre.

- **Agent d’exécution** : la règle désigne l’agent dont les droits encadrent le hook
  qu’elle écoute et les outils qu’elle appelle elle-même ; son créateur doit y avoir
  accès. Un membre ne peut donc pas écouter une instance que ses agents n’ont pas le
  droit d’écouter. Les droits sur les hooks et les outils n’existent que sur les
  agents.
- **Blocs** : si / sinon imbriqués et boucles.
- **Conditions** : un éditeur structuré de groupes ET / OU imbriqués, d’opérateurs sur
  tout champ de la payload (égal, contient, expression régulière, dans une liste,
  présent) et de prédicats de Cairn (l’auteur fait partie de personnes choisies,
  l’auteur a démarré la conversation d’une clé, une conversation existe pour une clé,
  quota par auteur).
- **Formules** : toute valeur d’une action ou d’une condition peut être une formule en
  [CEL](https://cel.dev) (Common Expression Language), complétée par des fonctions de
  Cairn (tronquer, découper en morceaux, formater une date…). Une formule est pure,
  sans effet de bord ni boucle, et son coût d’évaluation est borné.
- **Actions** : démarrer un agent, orienter une conversation, appeler un outil de
  plugin (jamais un outil de Cairn comme `cairn_installation` ou la délégation),
  annuler la conversation d’une clé, répondre à la question d’un agent, mémoriser une
  valeur, attendre puis reprendre (sept jours au plus, avec un nombre borné de
  déclenchements en attente par règle). Chaque action peut utiliser la payload et le
  résultat des actions précédentes (`{{steps.post.messageId}}`) et ne fait qu’une
  chose : démarrer un agent avec une clé déjà liée échoue, et c’est l’arbre qui
  choisit d’orienter la conversation existante.
- **Clé de conversation** : les actions d’une règle n’atteignent une conversation
  existante que par sa clé. Démarrer un agent peut rattacher la conversation à une clé
  tirée de la payload (`discord:{{threadId}}`). Les clés appartiennent au créateur de
  la règle : deux membres qui utilisent la même clé désignent deux conversations
  différentes, et une règle n’atteint que les clés de son créateur. Une clé se libère
  quand sa conversation est supprimée ; une conversation terminée garde la sienne et
  peut être orientée de nouveau. Orienter, annuler, répondre à une question ou
  mémoriser une valeur sur la conversation d’une clé exige en plus que le créateur ait
  accès à l’agent de cette conversation.
- **Valeurs mémorisées** : soit sur une conversation, par sa clé, partagées par les
  règles qui la touchent et supprimées avec elle, soit propres à la règle.
- **Hooks de Cairn** : Cairn émet quatre hooks pour les conversations rattachées à une
  clé : réponse prête (texte et livrables), question posée, progression (environ
  toutes les 8 secondes, une étape courte sans détail, une seule en attente par
  conversation) et fin (terminée, échouée ou annulée). Une règle ne les reçoit que
  pour les clés de son créateur, et tant qu’il a accès à l’agent de la conversation.
- **Conversations démarrées** : une conversation démarrée par une règle a son créateur
  pour auteur, qui ne la rend pas privée
  ([ADR-0029](0029-sharing-whole-installations.md)) ; elle note l’auteur externe comme
  origine.
- **Versions** : un déclenchement garde la version de la règle avec laquelle il a
  commencé ; une modification ne vaut que pour les événements suivants.
- **Historique** : chacun voit l’état des déclenchements des règles qu’il voit ; leur
  détail (payload et résultats des actions) n’est visible que du créateur, du
  propriétaire et des membres qui ont accès à l’agent d’exécution.
- **Échecs** : une erreur dont le service confirme que rien n’a été fait (limite de
  débit, refus avant traitement) fait réessayer l’action en échec seule, avec un délai
  croissant qui respecte celui que demande le service ; les actions déjà faites ne
  sont pas rejouées. Une mutation interrompue ou sans réponse reste ambiguë et n’est
  jamais rejouée ([ADR-0056](0056-plugin-oauth-file-transfers-and-gmail.md)) : elle
  arrête le déclenchement. Une erreur définitive, ou des essais épuisés, arrête le
  déclenchement, avec le détail de l’erreur dans l’historique.

Un bot Discord devient un agent qui a le hook `message` et les outils `read`, `post`
et `react` limités à un salon, accompagné des règles du modèle « Bot
conversationnel », dont les conditions choisissent les auteurs Discord auxquels il
répond. Le plugin garde son transport, ses hooks, ses outils et ses ressources ; le
rendu des conversations Cairn dans Discord disparaît.

### Autorité des règles

Une règle s’exécute avec l’autorité de son créateur, revérifiée à chaque action : un
droit manquant pour une action la fait échouer. Seul le créateur modifie sa règle. Les
autres membres la voient sans pouvoir la modifier ; ils peuvent la dupliquer s’ils ont
tous les droits qu’elle utilise, le duplicata leur appartenant. Le propriétaire peut
supprimer n’importe quelle règle ; supprimer une règle arrête ses déclenchements en
cours. Comme les tâches ([ADR-0029](0029-sharing-whole-installations.md)), une règle
appartient à une adhésion, et un déclenchement exige une adhésion vérifiée récemment ;
si elle ne peut pas l’être, il attend.

Une règle ne se déclenche que si rien ne l’en empêche. Chaque empêchement se lève
indépendamment des autres :

| Empêchement | Cause | Levé par |
| --- | --- | --- |
| Désactivée | son créateur, ou une duplication (le duplicata démarre ainsi) ; arrête les déclenchements en cours | son créateur |
| Pause d’urgence | le propriétaire ; arrête aussi les déclenchements en cours | le propriétaire |
| Budget épuisé | un plafond horaire dépassé ; le créateur est prévenu | son créateur ou le propriétaire |
| Droits perdus | le créateur perd l’accès à l’agent d’exécution, ou cet agent le hook de plugin écouté | le retour du droit, de lui-même |
| Adhésion terminée | le créateur est retiré, part ou est supprimé ; le réinviter n’y change rien | jamais : il reste à dupliquer |

Un agent qui a l’accès à l’installation n’écrit des règles que dans une conversation
dont toutes les entrées, depuis sa création, ont été écrites par une même personne sur
le web, Android ou le MCP public : messages, orientations, réponses aux questions, et
modifications d’un message en attente, qui comptent comme une entrée de celui qui
modifie. Une conversation qu’une règle, un agent, `cairn_installation` ou une autre
personne a atteinte, une tâche (planifiée, lancée à la main ou déléguée) et une
mission ne le permettent jamais, même plus tard. Il écrit au nom de cette personne :
il crée des règles à son nom, et ne modifie, désactive, réactive ou supprime que les
règles de ce compte. Il ne peut accorder de droits à aucun agent ni à aucun compte, ni
toucher à la règle d’un autre compte, ni lever une pause d’urgence, ni réduire, sur
l’agent d’exécution d’une règle d’un autre compte dont l’adhésion dure, un droit sur
un hook ou un outil que cette règle utilise, ressources comprises, quel que soit
l’état de la règle. Une règle créée par le MCP public a pour créateur le propriétaire
qui a autorisé ce client.

Une règle n’atteint un agent dont la politique autorise l’exécution native, ou une
conversation qui s’exécute en natif, que si son créateur détenait l’autorité native
([ADR-0050](0050-trusted-native-execution.md)) dans la requête authentifiée qui a
enregistré cette version de la règle. Une requête de `cairn_installation`, ou du MCP
public sans autorité native, ne peut pas l’accorder, comme pour les tâches déléguées
([ADR-0059](0059-delegated-tasks.md)). L’autorité est conservée avec la version ; à
chaque action qui résout, au moment du déclenchement, un agent ou une conversation
natifs, Cairn vérifie à la fois cette autorité conservée et l’autorité native actuelle
du créateur ; sans elles, l’action échoue. Le travail natif qu’une règle démarre garde
les limites de node et le renouvellement de bail de tout travail natif.

### Budget du moteur

Le moteur est borné pour qu’une boucle (une action qui déclenche le hook qui l’a
lancée) s’arrête d’elle-même. Cairn ne peut pas toujours relier un appel d’outil à
l’événement qu’il provoque en retour chez le service ; le budget est donc porté par
chaque règle : un plafond de déclenchements et un plafond de conversations démarrées
ou orientées par heure, avec des valeurs par défaut que le créateur peut changer sous
un maximum fixé par Cairn. Au dépassement, la règle est suspendue jusqu’à ce que son
créateur ou le propriétaire la relance, et son créateur est prévenu ; cette suspension
laisse finir les déclenchements en cours, que bornent leurs propres limites. Les
chaînes que Cairn sait suivre, quand une action provoque un hook de Cairn, ont en plus
une profondeur maximale. Chaque déclenchement est limité en nombre de pas (blocs et
itérations) et d’appels d’outils, sans compter les attentes ; au dépassement, il
s’arrête en erreur.

Un déclenchement qui se termine sans effet (aucune action exécutée en dehors des blocs
et des conditions) ne compte pas dans les plafonds horaires et n’est pas gardé dans
l’historique, qui n’en garde que le nombre : un salon actif n’épuise pas le budget de
la règle. Les déclenchements par la progression, déjà cadencée par Cairn, ne comptent
pas dans le plafond de déclenchements ; les conversations qu’ils démarrent ou
orientent comptent toujours.

### Sans migration

Les bots et délégations existants sont supprimés plutôt que convertis : la seule
installation qui les utilise est celle du propriétaire du projet. Leur suppression
révoque leur travail en cours et ses descendants. Les droits existants d’un agent sur
une instance deviennent un accès complet à cette instance, qui comprend désormais ses
hooks : par choix du propriétaire, les membres peuvent donc écouter dès la mise en
place les instances que leurs agents utilisent, jusqu’à ce qu’il réduise ces droits.
L’instance qui fournit aujourd’hui les identifiants de run d’un agent devient
l’instance choisie pour eux.

## Options écartées

- **Garder la délégation par instance** : spécifique au bot conversationnel et
  contraire au modèle où l’agent porte ses droits.
- **Donner des droits à des identités externes** : inutile dès qu’une règle agit avec
  l’autorité de son créateur ; examiner l’auteur suffit.
- **Donner aux comptes des droits directs sur les outils** : un second système de
  permissions à côté de celui des agents.
- **Seulement des droits sur l’instance entière** : impossible alors de donner un
  salon sans donner tout le serveur.
- **Un langage de formules propre à Cairn, ou des expressions Luau** : le premier
  serait à écrire et maintenir, les secondes rapprochent les règles du code et
  alourdissent chaque évaluation. CEL est standard, borné et a une implémentation Rust
  (`cel-interpreter`).
- **Des filtres séparés de l’arbre** : écartés au profit d’un seul arbre par règle.
- **Une suite d’actions linéaire** : il aurait fallu doubler les règles avec des
  conditions opposées pour chaque alternative.
- **Seulement des hooks et des outils, sans règles** : chaque message déclencherait un
  agent, sans moyen de répondre de façon fiable.

## Conséquences

Les restrictions de ressource reposent sur ce que le package déclare : les champs qui
désignent des ressources et la liste qui fournit leur hiérarchie. Un package approuvé
reste donc de confiance pour ces déclarations, comme il l’était pour l’admission.

Une règle qui ne teste pas l’auteur ouvre son agent à n’importe qui : l’éditeur
prévient son créateur quand l’agent démarré a des droits de plugin ou l’accès à
l’installation, comme le faisait l’assistant des bots
([ADR-0068](0068-owner-chosen-agents-for-bot-delegation.md)).

Les risques d’un droit trop large relèvent du propriétaire ; Cairn doit en revanche
rendre ces droits lisibles et n’en accorder aucun par défaut. Les capacités d’un
plugin qui ne passent pas par des entrées d’outil ne peuvent pas être restreintes : un
outil doit recevoir en entrée ce qu’il touche.

Un agent démarré par une règle agit pour son créateur, à partir d’un contenu écrit par
des inconnus. Il ne peut pas écrire de règles, mais s’il a l’accès à l’installation,
un auteur externe peut tenter de lui faire faire tout le reste de ce que permet cet
accès, y compris supprimer l’agent ou l’instance dont dépendent les règles d’autres
comptes : un agent qui répond à un salon public ne devrait pas l’avoir. De même, dans
un tour qu’une personne a lancé, un agent qui lit un contenu externe par un outil (un
salon, une issue) peut être poussé à écrire une règle durable au nom de cette
personne ; ce risque résiduel est accepté, et l’historique d’une règle montre dans
quelle conversation elle a été écrite.

Comme toute capacité de l’interface, les règles (éditeur compris) et les droits par
ressource arrivent sur le web, Android, le MCP public et `cairn_installation` en même
temps, avec les limites ci-dessus : `cairn_installation` n’accorde aucun droit, et
l’accès des comptes aux agents reste au web et à Android.
