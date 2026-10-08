# Partage d’une installation entière, sans conversations privées

Date : 2026-10-03.

Le propriétaire partage une installation entière en invitant d’autres comptes Leo par e-mail. Il n’existe que deux rôles : le propriétaire, qui gère les nodes, les comptes d’agent de code, les secrets et le partage, et le membre, qui fait travailler les agents. Chaque membre voit toutes les conversations de l’installation. L’interface montre une seule installation courante à la fois.

Les autres options étaient un rôle administrateur ou lecteur supplémentaire, et des conversations privées à leur auteur. Les conversations privées auraient exigé un auteur sur chaque donnée de l’installation et un contrôle d’accès dans chaque requête, alors qu’aucune donnée de l’installation n’a aujourd’hui de propriétaire. Un membre fait déjà travailler les agents avec les abonnements et les secrets du propriétaire : le partage exprime une confiance de même niveau que celle accordée aux nodes.

Conséquences difficiles à inverser :

- Les données d’une installation restent sans auteur. Ajouter plus tard des conversations privées demandera d’attribuer les conversations existantes.
- Retirer un membre lui retire l’accès, mais ne masque pas ce qu’il a déjà vu ni les conversations qu’il a créées.

## Complément : tâches et retrait d'un membre (2026-10-05, #59)

Les conversations restent communes à toute l'installation. En revanche, chaque
nouvelle tâche devra conserver le compte Leo de son auteur pour identifier les
engagements récurrents pris avec les comptes d'agent de code du propriétaire.
Les tâches existantes sans auteur seront considérées comme celles du propriétaire.

Le retrait ou le départ d'un membre devra désactiver les tâches planifiées dont
il est l'auteur avant de terminer le retrait. Les exécutions déjà admises pourront
finir ; le retrait ne supprime ni les tâches ni leur travail. La suppression des
tâches sera réservée au propriétaire, y compris pour les tâches d'un membre.

L'autre option consistait à laisser tourner tous les engagements planifiés
après le retrait. Elle ne permettrait pas au propriétaire de mettre fin à la
consommation future de ses abonnements en retirant un membre. L'auteur des tâches
sert uniquement à cette gestion des engagements ; il n'introduit ni conversations
privées ni nouveau rôle. La mise en œuvre de ce complément appartient à la suite
de #59 après les bloquants de déploiement public.

## Complément : engagements et liens publics (2026-10-07, #59)

L'auteur d'une tâche est le compte vérifié à sa création et reste inchangé quand
un autre membre ou le propriétaire la modifie. Les tâches anciennes sans auteur,
et celles créées par un agent avec ses droits internes, sont attribuées au
propriétaire. L'auteur ne rend aucune conversation privée.

Un engagement appartient à une adhésion précise : retirer, faire partir ou
supprimer le compte d'un membre met fin à ses engagements planifiés. Le compte
reste enregistré comme auteur. Réinviter le même compte ne relance pas ses anciens
engagements. Pour reprendre une planification, un compte autorisé duplique la tâche ;
il devient l'auteur de cette nouvelle tâche. Les anciennes tâches restent visibles,
modifiables et exécutables manuellement. Seul le propriétaire peut supprimer une
tâche, quel que soit son auteur.

L'installation consulte l'autorité officielle avant de créer ou modifier une tâche,
avant une admission planifiée et avant de reconnecter son relais. Elle ne conserve
pas une liste de membres faisant autorité. Le retrait demande aussi une réconciliation
immédiate à l'installation en ligne. Si l'installation est hors ligne ou si cette
demande échoue, le prochain contrôle précède toute nouvelle admission planifiée.
Une indisponibilité temporaire reporte les nouvelles admissions, sans désactiver
définitivement les engagements encore valides. Les exécutions admises avant le
retrait continuent. Cela préfère une planification retardée à une consommation
future impossible à révoquer pendant une partition réseau.

Le propriétaire contrôle la création et la révocation des liens publics dans
l'interface partagée, y compris pour les livrables des membres et les liens anciens.
Les membres peuvent consulter et copier un lien déjà publié. Le retrait d'un membre
ne révoque pas automatiquement ces liens : le propriétaire conserve la publication
et peut la révoquer explicitement. Les liens existants n'ont pas d'auteur fiable ;
les supprimer en masse au retrait ferait perdre des publications légitimes. Les
autorisations explicites des agents, limitées à leur exécution, restent applicables.
Les copies déjà téléchargées restent évidemment chez leurs destinataires.

## Complément : modification et indisponibilité (2026-10-08, revue #116)

L'auteur reste immuable. Seul cet auteur ou le propriétaire peut modifier le
contenu d'une tâche ou la remettre en marche. Un membre peut seulement mettre
en pause ou archiver la tâche d'un autre auteur, sans changer ses autres champs.
Cela inclut les tâches anciennes attribuées au propriétaire. Pour prendre un
engagement différent, le membre duplique la tâche et devient auteur de la copie.
Réattribuer l'auteur à chaque modification a été écarté : une simple correction
par le propriétaire pourrait autrement faire survivre l'engagement d'un membre
à son retrait.

La pause et l'archivage seuls ne demandent pas de contrôle de l'autorité officielle :
ils réduisent le travail futur et restent possibles pendant son indisponibilité.
Les nouvelles admissions planifiées attendent toujours un contrôle courant,
y compris après leur préparation. Leur boucle est indépendante du lancement et
de la reprise des travaux déjà admis : une attente officielle ne les retarde pas.
Le refus d'une admission pour défaut d'autorité est indiqué sur la tâche ; une
erreur de préparation propre à une tâche est journalisée et avance son échéance,
sans empêcher les autres tâches de passer.
