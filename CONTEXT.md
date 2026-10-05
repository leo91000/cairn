# Leo Agent Manager

Leo Agent Manager permet de conduire des conversations avec des agents et de retrouver leur travail.

## Language

**Service officiel** :
Le service hébergé par Leo qui fournit l’interface web et mobile, les comptes Leo et l’accès à toutes les installations d’un compte. Il ne conserve pas le travail des agents.

**Compte Leo** :
L’identité d’une personne sur le service officiel, définie par une adresse e-mail vérifiée. Le code par e-mail, Google, GitHub et les passkeys ne sont que des moyens de s’y connecter. C’est la seule manière d’accéder à une installation.
_Avoid_ : utilisateur GitHub, compte d’agent de code

**Installation** :
Un ensemble autonome déployé par une personne sur ses machines, avec ses nodes d’exécution. Il conserve les conversations, les projets, les comptes d’agent de code et les secrets ; ces données ne quittent pas l’installation pour être conservées par le service officiel.
_Avoid_ : node, set de nodes, instance, serveur

**Revendication d’une installation** :
Le rattachement d’une installation à un compte Leo, qui en devient le propriétaire. Une installation non revendiquée n’est accessible à personne.

**Propriétaire d’une installation** :
Le compte Leo auquel l’installation est rattachée. Il est le seul à gérer ses nodes, ses comptes d’agent de code, ses secrets et son partage.
Sans propriétaire, une installation redevient non revendiquée ; la propriété ne se transfère pas.

**Autorisation MCP d’une installation** :
L’accès qu’un propriétaire accorde à un client externe pour une seule installation et des droits précis. Les membres font travailler les agents depuis l’app, sans pouvoir accorder ni utiliser cet accès externe.

**Membre d’une installation** :
Un compte Leo invité par le propriétaire. Il voit toutes les conversations de l’installation et fait travailler ses agents, donc avec les comptes d’agent de code et les secrets du propriétaire, sans pouvoir les gérer.
_Avoid_ : invité, collaborateur

**Installation courante** :
L’installation dans laquelle on travaille dans l’interface ; on n’en voit qu’une à la fois et on passe de l’une à l’autre.

**Relais** :
Le passage par le service officiel de tous les échanges entre l’interface et une installation, ouvert par l’installation elle-même. Le relais transporte le travail des agents sans le conserver.

**Node d’exécution** :
Machine rattachée à une installation pour y effectuer le travail d’un agent, selon ses capacités et les autorisations accordées. Une node peut être disponible de manière intermittente.
_Avoid_ : agent (qui désigne l’agent chargé du travail, et non la machine)

**Capacité d’une node** :
Possibilité de travail vérifiée sur une node, distincte de la seule présence du matériel et de sa disponibilité au moment de la demande. Une capacité ne donne aucun droit supplémentaire à l’agent.

**Tag de node** :
Étiquette servant à décrire ou sélectionner une node parmi celles autorisées pour l’agent. Un tag ne constitue pas une autorisation d’accès aux projets ou aux comptes.

**Réservation de ressources** :
Part des ressources d’une node attribuée à une exécution dans les plafonds configurés. Elle est prise en compte avant d’accepter d’autres travaux sur cette node.

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
Une conversation supprimée par l’utilisateur, récupérable pendant 30 jours avant son effacement définitif et celui de ses données associées.

**Récupération depuis la corbeille** :
Retour d’une conversation à l’état actif, sans relancer l’agent, les envois annulés ni les liens publics révoqués.

**Agent de code** :
Le moteur qui exécute le travail d’un agent : Codex (OpenAI) ou Claude Code (Anthropic). Chaque agent en choisit un, et une conversation peut en changer d’un message à l’autre.
_À éviter_ : fournisseur, driver, assistant de code.

**Compte d’agent de code** :
Un abonnement connecté à un agent de code (un compte ChatGPT pour Codex, un compte Claude pour Claude Code). Les exécutions consomment son usage. Un agent de code peut avoir plusieurs comptes ; un compte en pause n’est plus choisi pour les nouvelles exécutions.
_À éviter_ : connexion Claude, compte Codex (sauf pour désigner un compte de cet agent de code).

**Fenêtre d’usage** :
La période sur laquelle l’agent de code plafonne l’usage d’un compte : 5 heures, semaine, ou semaine limitée à un modèle.

**Capacité restante** :
La part d’usage restante la plus basse parmi les fenêtres d’usage qui s’appliquent à un modèle. Une nouvelle exécution prend le compte disponible qui a le plus de capacité restante ; c’est le compte **prochain**.

**Exécutions parallèles** :
Le nombre maximal d’exécutions simultanées sur un compte. Le réduire laisse finir les exécutions en cours.

**Réinitialisation en réserve** :
Une remise à zéro de fenêtre d’usage offerte par Codex, utilisée automatiquement quand la capacité restante d’un compte actif tombe à 2 %. Elle n’existe pas pour Claude Code.

**Auteur d’une tâche** :
Le compte Leo qui a créé une tâche. Le retrait de cet auteur met fin aux déclenchements planifiés de ses tâches ; leur travail reste partagé avec toute l’installation.

