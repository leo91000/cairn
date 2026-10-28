# Cairn

Cairn permet de conduire des conversations avec des agents et de retrouver leur travail.

## Language

**Placement automatique** :
Le choix d’un node autorisé et disponible pour une nouvelle conversation, qu’il exécute des VM ou des processus natifs. Le type d’environnement choisi est conservé pendant la conversation, sauf migration explicite.

**Priorité de nodes d’un agent** :
L’ordre de préférence propre à un agent pour placer ses nouvelles conversations. Un node prioritaire indisponible laisse la place au suivant, puis aux autres nodes autorisés. Une préférence ne donne aucun droit d’accès.

**Beacon** :
Le service hébergé par Cairn qui fournit l’interface web et mobile, les comptes Cairn et l’accès à toutes les installations d’un compte. Il ne conserve pas le travail des agents.
Il peut conserver les diagnostics techniques de l’application, sans contenu des conversations, pour améliorer ses performances.

**Compte Cairn** :
L’identité d’une personne sur Beacon, définie par une adresse e-mail vérifiée. Le code par e-mail, Google, GitHub et les passkeys ne sont que des moyens de s’y connecter. Cette identité est requise pour accéder à l’interface et à l’API d’une installation. Une personne externe, par exemple l’auteur d’un message Discord, n’a aucun droit : elle n’est qu’une donnée d’un hook.
_Avoid_ : utilisateur GitHub, compte d’agent de code

**Installation** :
Un ensemble autonome déployé par une personne sur ses machines, avec ses nodes d’exécution. Il conserve les conversations, les projets, les comptes d’agent de code et les secrets ; ces données ne quittent pas l’installation pour être conservées par Beacon.
_Avoid_ : node, set de nodes, instance, serveur

**Revendication d’une installation** :
Le rattachement d’une installation à un compte Cairn, qui en devient le propriétaire. Une installation non revendiquée n’est accessible à personne.

**Révocation définitive d’une installation** (« Revoke and forget installation ») :
La suppression de son rattachement et de son identité auprès de Cairn, décidée par son propriétaire, qui met fin à ses accès et à son partage tout en conservant ses données sur la machine. Revenir exige une nouvelle revendication, sous une nouvelle identité d’installation.
_Avoid_ : détachement (qui conserve l’identité de l’installation)

**Propriétaire d’une installation** :
Le compte Cairn auquel l’installation est rattachée. Il est le seul à gérer ses nodes, ses comptes d’agent de code, ses secrets et son partage.
Sans propriétaire, une installation redevient non revendiquée ; la propriété ne se transfère pas.

**Autorisation MCP d’une installation** :
L’accès qu’un propriétaire accorde à un client externe pour une seule installation et des droits précis. Les membres font travailler les agents depuis l’app, sans pouvoir accorder ni utiliser cet accès externe.

**Membre d’une installation** :
Un compte Cairn invité par le propriétaire. Il voit toutes les conversations de l’installation et fait travailler ses agents, donc avec les comptes d’agent de code et les secrets du propriétaire, sans pouvoir les gérer.
_Avoid_ : invité, collaborateur

**Installation courante** :
L’installation dans laquelle on travaille dans l’interface ; on n’en voit qu’une à la fois et on passe de l’une à l’autre.

**Occupation du contexte** :
La part de la fenêtre de contexte du modèle qu’occupait la dernière requête de l’agent de code, telle que celui-ci la mesure ou l’estime. Elle n’est jamais déduite de la consommation cumulée d’une conversation ; sans mesure, elle reste indisponible.
_Avoid_ : consommation, usage cumulé

**Compaction du contexte** :
Le résumé, par l’agent de code, de son contexte antérieur pour libérer de la place dans sa fenêtre. Elle ne modifie pas l’historique visible de la conversation et n’est pas un message de l’utilisateur.

**Relais** :
Le passage par Beacon des échanges entre l’interface et une installation, sur la connexion sortante ouverte par l’installation. C’est la voie de secours quand la connexion directe est impossible ou interrompue ; il transporte le travail des agents sans le conserver.
_Avoid_ : tunnel (qui désigne la connexion technique de l’installation)

**Connexion directe** :
L’échange entre l’interface et une installation sans passer par Beacon, établi quand le réseau le permet et autorisé par l’installation pour un compte Cairn, une session et un rôle. Elle transporte les mêmes échanges que le relais et bascule sur lui si elle tombe.
_Avoid_ : accès local, P2P

**Signalisation** :
La mise en relation par Beacon d’une interface et d’une installation pour établir une connexion directe. Elle transporte l’autorisation et les paramètres de connexion, jamais le contenu des conversations.

**Autorisation de connexion directe** :
La permission signée par Beacon, limitée à une installation, un compte, une session et quelques minutes, sans laquelle l’installation refuse une connexion directe. Sa révocation ou son échéance ferme la connexion.

**Passerelle de node** :
Service local d’une machine ajoutée sans adresse du manager, par lequel sa node joint le manager : directement quand les réseaux le permettent après une mise en relation par Beacon, sinon par Beacon, toujours chiffré jusqu’au **certificat de node** du manager. Elle ne détient aucun droit : la node et le manager s’authentifient toujours entre eux.
_Avoid_ : proxy, tunnel de node

**Certificat de node** :
Certificat persistant du manager sur lequel se terminent les connexions des nodes ajoutées sans adresse. Son empreinte figure dans la commande d’installation, et la passerelle refuse tout autre certificat.
_Avoid_ : certificat TLS du manager

**Node d’exécution** :
Machine rattachée à une installation pour y effectuer le travail d’un agent, selon ses capacités et les autorisations accordées. Une node peut être disponible de manière intermittente.
_Avoid_ : agent (qui désigne l’agent chargé du travail, et non la machine)

**Exécution native** :
Travail d’un agent effectué par le système de la node, avec ses pilotes et les ressources partagées avec son propriétaire. Il exige des agents sélectionnés et des personnes explicitement autorisées, avec une frontière d’isolation plus faible qu’une VM.

**Migration du mode d’exécution d’une conversation** :
Action explicite qui transfère les fichiers durables entre VM Firecracker et Linux natif après configuration de l’agent, en conservant le même fil et son historique. L’exécution s’arrête ; le prochain message ouvre une nouvelle session du moteur avec le contexte du fil. Les dépendances système peuvent nécessiter une réinstallation. L’autorisation explicite du propriétaire permet de reprendre le travail existant avec ses droits natifs ; une délégation reste bornée par ses droits actuels.

**Capacité d’une node** :
Possibilité de travail vérifiée sur une node, distincte de la seule présence du matériel et de sa disponibilité au moment de la demande. Une capacité ne donne aucun droit supplémentaire à l’agent.

**Tag de node** :
Étiquette servant à décrire ou sélectionner une node parmi celles autorisées pour l’agent. Un tag ne constitue pas une autorisation d’accès aux projets ou aux comptes.

**Réservation de ressources** :
Part des ressources d’une node attribuée à une exécution dans les plafonds configurés. Elle est prise en compte avant d’accepter d’autres travaux sur cette node.
Pour le disque natif compatible, le manager protège aussi la capacité accordée mais encore inutilisée ; il peut l’agrandir automatiquement par incréments dans le budget commun. Le workspace et le `/tmp` géré consomment la même capacité.

**Déplacement d’une conversation** :
Changement de node d’exécution d’une même conversation, avec une pause puis une reprise de sa session et de son environnement de travail. Il ne conserve pas les processus en cours ni leur mémoire.

**Environnement de travail d’une conversation** :
Ensemble des fichiers, outils installés et données conservés pour le travail d’une conversation. Il comprend les modifications non publiées des projets et les données nécessaires à leur reprise, au-delà du seul historique des messages.

**Reprise à la demande** :
Remise en activité d’une conversation dont l’environnement de travail devient accessible progressivement, au fil des besoins de l’agent. Elle permet de reprendre le travail avant que l’ensemble de cet environnement soit disponible sur la node d’exécution.

**Travail non synchronisé** :
Part du travail d’une conversation encore conservée uniquement sur sa node d’exécution. Sa récupération dépend de la conservation de cette node jusqu’à la fin de la synchronisation.

**État publié** :
Dernier état cohérent de l’environnement d’une conversation disponible à distance pour sa reprise sur une autre node. Il ne constitue pas un historique permettant de revenir à des versions antérieures.

**Synchronisation d’une conversation** :
Mise à disposition à distance de son environnement de travail courant. Elle progresse en arrière-plan ; son retard indique le travail encore exposé à la perte de la node d’origine.

**Conversation à la corbeille** :
Une conversation supprimée par l’utilisateur, masquée pendant l’effacement définitif de ses données associées. Les nouvelles suppressions sont irréversibles immédiatement ; seules les conversations déjà à la corbeille avant ce changement conservent leur délai de récupération initial.

**Récupération depuis la corbeille** :
Retour d’une conversation à l’état actif, sans relancer l’agent, les envois annulés ni les liens publics révoqués.

**Agent de code** :
Le moteur qui exécute le travail d’un agent : Codex (OpenAI), Claude Code (Anthropic) ou OpenCode. Chaque agent en choisit un, et une conversation peut en changer d’un message à l’autre. Quand un quota bloque la demande en cours, l’utilisateur peut choisir un autre agent de code disposant d’un compte disponible pour reprendre cette même demande avec son historique et ses fichiers.
_À éviter_ : fournisseur, driver, assistant de code.

**Compte d’agent de code** :
Un abonnement connecté à un agent de code (un compte ChatGPT pour Codex, un compte Claude pour Claude Code) ou une clé API liée à un fournisseur de modèles pour OpenCode (par exemple OpenCode Zen). Les exécutions consomment son usage. Un agent de code peut avoir plusieurs comptes ; un compte en pause n’est plus choisi pour les nouvelles exécutions.
_À éviter_ : connexion Claude, compte Codex (sauf pour désigner un compte de cet agent de code).

**Fenêtre d’usage** :
La période sur laquelle l’agent de code plafonne l’usage d’un compte : 5 heures, semaine, ou semaine limitée à un modèle.

**Capacité restante** :
La part d’usage restante la plus basse parmi les fenêtres d’usage qui s’appliquent à un modèle. Une nouvelle exécution prend le compte disponible qui a le plus de capacité restante ; c’est le compte **prochain**.

**Exécutions parallèles** :
Le nombre maximal d’exécutions simultanées sur un compte. Le réduire laisse finir les exécutions en cours.

**Réinitialisation en réserve** :
Une remise à zéro de fenêtre d’usage offerte par Codex, utilisée automatiquement quand la capacité restante d’un compte actif tombe à 2 %. Elle n’existe pas pour Claude Code.

**Vitesse** :
Le niveau de service Codex demandé pour un tour (Standard, Fast, Ultrafast…), choisi indépendamment du modèle et du raisonnement pour le prochain message ou par défaut sur un agent. Seuls les niveaux que le catalogue Codex d’un compte annonce pour le modèle sont proposés ; une vitesse plus rapide consomme plus vite l’usage du compte. Claude Code et OpenCode n’en proposent pas.
_À éviter_ : tier, mode rapide (sauf pour nommer le niveau Fast de Codex).

**Question d’un agent** :
Une demande de réponse adressée à une personne par un agent dans une conversation Cairn. Elle peut proposer des choix ou laisser la personne s’exprimer librement.

**Questionnaire d’un agent** :
Un ensemble d’une ou plusieurs questions d’un agent auxquelles une personne répond en un seul envoi. Il reste disponible dans la conversation pendant l’attente des réponses.

**Commentaire de réponse** :
Le texte libre qu’une personne ajoute aux choix retenus pour une question d’un agent, afin de préciser sa réponse.

**Demandeur d’une mission** :
La conversation ou la mission dont l’agent a lancé l’exécution d’une mission. Une mission bloquée qui attend son demandeur lui revient plutôt qu’à l’utilisateur, tant que ce demandeur existe.
_Avoid_ : parent, orchestrateur

**Tâche déléguée** :
Une conversation qu’un agent autorisé à déléguer crée depuis une conversation,
éventuellement sur un autre agent de code et un autre modèle, pour un travail qui
lui revient. Elle garde sa conversation demandeuse, s’exécute dans son propre
espace avec les droits de son agent, que l’agent demandeur doit couvrir, et ne
délègue pas à son tour.
_Avoid_ : sous-agent (réservé aux sous-agents natifs d’un agent de code), parent, enfant

**Mise de côté d’un résultat** :
Le choix d’un compte de retirer de « Pour vous » le résultat bloqué d’une mission : écarté jusqu’à son prochain résultat, ou reporté jusqu’à une heure donnée. Chaque membre met de côté pour lui seul.
_Avoid_ : archivage (qui retire la mission elle-même)

**Auteur d’une tâche** :
Le compte Cairn qui a créé une tâche. Le retrait de cet auteur met fin aux déclenchements planifiés de ses tâches ; leur travail reste partagé avec toute l’installation.

**Skill** :
Un savoir-faire réutilisable que les agents appliquent : ses instructions et les fichiers qui les accompagnent (références, modèles, scripts, images). Une skill est globale ou propre à un projet ; une conversation ou une exécution reçoit les skills sélectionnées telles qu’elles étaient à sa création.
_Avoid_ : prompt, plugin

**Source d’une skill** :
L’emplacement d’où une skill a été importée par la source de skills d’un plugin (par exemple un dossier d’un dépôt GitHub), avec la version importée. La source permet de comparer la skill à une version plus récente et de la remplacer par celle-ci ; les modifications faites dans Cairn depuis l’import sont alors perdues.

**Instance de plugin** :
Une configuration d’un plugin par le propriétaire, par exemple un bot Discord, un
compte de service 1Password ou un compte GitHub : ses réglages et ses secrets
chiffrés. Les droits des agents sur l’instance, complets ou outil par outil, se
choisissent sur la page de chaque agent, ou à la dernière étape de sa création, qui
ne fait qu’ajouter des droits. Aucun agent n’est autorisé par défaut.
_Avoid_ : connexion (réservé aux comptes des agents de code)

**Assistant de configuration d’un plugin** :
Les étapes que le propriétaire suit pour ajouter ou modifier une instance, dans
l’ordre où chacune devient possible : ce qu’il crée d’abord chez le service (par
exemple une app Google Cloud), les réglages, la connexion, les personnes et les
agents. Le catalogue annonce ce que la connexion demande : se connecter, coller un
jeton ou créer sa propre app. Une étape partagée, comme le client OAuth d’une app,
se configure une fois et se réutilise pour les instances suivantes du même plugin.
_Avoid_ : wizard, onboarding

**Contribution d’un plugin** :
Ce qu’un package déclare dans son manifeste et que Cairn affiche et contrôle : le
formulaire d’une instance (que le package peut compléter pendant la saisie), la
connexion par appareil, les commandes et outils donnés aux agents autorisés, leurs
instructions, les identifiants de run, les conditions d’attente, les sources de
projets et de skills, le panneau d’une instance et son test. Le code du package ne
fait que répondre ; il ne reçoit jamais les secrets.

**Connexion par appareil** :
La connexion d’une instance à un service tiers que Cairn déroule pour le propriétaire :
il saisit sur la page du service le code que Cairn affiche, et Cairn garde le jeton
obtenu comme identifiant de l’instance, sans le montrer au navigateur ni au package.
_Avoid_ : OAuth (le protocole), login

**Identifiants de run** :
Ce que Cairn écrit dans l’environnement d’une exécution pour un agent autorisé sur une
instance : fichiers de configuration d’outils, identifiants Git et identité de commit,
rendus depuis l’instance. Un droit retiré les efface à la reprise suivante ; la fin de
l’exécution les supprime. Un agent reçoit ceux d’une seule instance par plugin.

**Source de projets** :
Ce qu’un plugin propose d’importer comme projets à travers une instance, par exemple
les dépôts d’un compte GitHub. Cairn clone le projet choisi avec les identifiants de
l’instance et retient l’instance comme source du projet.

**Condition d’un plugin** :
Ce qu’un agent autorisé peut attendre avec `wait_for` grâce à un plugin, par exemple
les checks requis d’une pull request. Cairn garde l’attente et reprend la
conversation ; le plugin dit seulement si la condition est remplie.

**Transport d’un plugin** :
La connexion qu’un package entretient lui-même avec un service externe, par exemple la
Gateway de Discord : un worker dédié par instance pilote un socket que Cairn ouvre vers
les hôtes déclarés, tient les obligations du protocole du service et émet les hooks du
plugin.
_Avoid_ : transport natif

**Hook d’un plugin** :
Un événement qu’un plugin, ou Cairn pour ses conversations, déclare pouvoir émettre,
avec ce qu’il transporte : par exemple un message écrit dans un salon Discord, avec son
auteur et son fil. Un hook ne fait rien seul ; ce sont les règles de hook qui y réagissent.
_Avoid_ : webhook (le mécanisme HTTP), trigger

**Règle de hook** :
Ce que le propriétaire décide, sans code, de faire quand un hook se produit : des
filtres puis une suite d’actions (déclencher un agent, orienter une conversation,
appeler un outil). Elle agit avec l’autorité de son créateur, seul à pouvoir la
modifier. Un plugin peut proposer des modèles de règles prêts à l’emploi.
_Avoid_ : hook (l’événement auquel elle réagit), automation, workflow, réponse

**Agent d’exécution d’une règle** :
L’agent dont les droits encadrent les outils qu’une règle de hook appelle elle-même.
Le créateur de la règle doit y avoir accès.

**Ressource d’un plugin** :
Ce sur quoi portent les hooks et les outils d’un plugin, et que le plugin sait lister :
un serveur ou un salon Discord, une personne, un dépôt GitHub. Le droit d’un agent sur
un outil peut être limité à certaines ressources.
_Avoid_ : scope, portée

**Clé de conversation** :
L’identifiant externe, tiré d’un hook, auquel une règle rattache une conversation, par
exemple un fil Discord, pour la retrouver et l’orienter lors des hooks suivants.
_Avoid_ : thread, binding

**Programme déclaré** :
Un exécutable du serveur qu’un package liste dans son manifeste et que le propriétaire
approuve avec lui. Cairn le lance sans shell, avec un environnement vide et, au besoin,
les secrets de l’instance en variables d’environnement. Il s’exécute hors du bac à
sable Luau.
