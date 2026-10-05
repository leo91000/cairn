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
