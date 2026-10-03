# Partage d’une installation entière, sans conversations privées

Date : 2026-10-03.

Le propriétaire partage une installation entière en invitant d’autres comptes Leo par e-mail. Il n’existe que deux rôles : le propriétaire, qui gère les nodes, les comptes d’agent de code, les secrets et le partage, et le membre, qui fait travailler les agents. Chaque membre voit toutes les conversations de l’installation. L’interface montre une seule installation courante à la fois.

Les autres options étaient un rôle administrateur ou lecteur supplémentaire, et des conversations privées à leur auteur. Les conversations privées auraient exigé un auteur sur chaque donnée de l’installation et un contrôle d’accès dans chaque requête, alors qu’aucune donnée de l’installation n’a aujourd’hui de propriétaire. Un membre fait déjà travailler les agents avec les abonnements et les secrets du propriétaire : le partage exprime une confiance de même niveau que celle accordée aux nodes.

Conséquences difficiles à inverser :

- Les données d’une installation restent sans auteur. Ajouter plus tard des conversations privées demandera d’attribuer les conversations existantes.
- Retirer un membre lui retire l’accès, mais ne masque pas ce qu’il a déjà vu ni les conversations qu’il a créées.
