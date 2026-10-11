# Accès explicite d’un agent à l’installation courante

Date : 2026-10-11.

## Constat

Les agents ont un canal MCP privé (ADR-0035), mais consulter les missions et les
exécutions exigeait de configurer Cairn comme connexion externe, avec un jeton
personnel ou OAuth. Ce détour ne représentait pas les permissions de l’agent.

## Décision

Le propriétaire peut cocher l’accès à l’installation courante dans l’éditeur
d’un agent, sur le web et Android. `access.installation` vaut `false` par défaut,
y compris pour Main et les agents existants. Les anciens clients qui omettent
ce champ lors d’une modification conservent sa valeur enregistrée.

L’option ajoute `cairn_installation` aux configurations Codex et Claude Code.
`/mcp-installation` réutilise le canal privé et le jeton de l’exécution. Le droit
est enregistré dans le grant : connaître l’URL, ou cocher l’option après émission,
ne confère aucun accès. Le manager vérifie l’exécution active, le tour courant,
l’absence d’annulation et les permissions actuelles avant chaque requête.
Le canal refuse le jeton d’une autre exécution, sur la node locale comme distante.

Le catalogue reprend les contrats publics pour consulter les agents et projets
autorisés, gérer les missions et lire ou annuler leurs exécutions. Il exclut
l’administration des agents, projets, skills, comptes et connexions MCP.
Les missions et exécutions doivent utiliser l’agent appelant : un autre agent
peut avoir des identifiants GitHub ou des grants 1Password différents malgré une
politique d’accès identique. Une mission ne peut déléguer des droits de ressources,
outils, sandbox ou nodes plus larges que ceux accordés à l’appelant. Les listes filtrent les ressources
avant pagination ; les lectures et mutations revérifient l’accès dans leur
transaction, après les étapes asynchrones de préparation.

Cet accès est une délégation explicite du propriétaire à l’agent. Comme avec le
MCP de gestion existant, les nouvelles missions planifiées ont le propriétaire
pour auteur et suivent la réconciliation normale de son accès. Les membres
peuvent faire travailler les agents autorisés depuis l’app, sans accorder un
accès MCP externe ni administrer leurs permissions. La règle du glossaire pour
l’autorisation MCP externe reste inchangée.

Le MCP externe de Beacon reste limité à une installation et aux scopes accordés
par son propriétaire. Aucun jeton personnel n’est copié dans l’environnement de
l’agent, et le pare-feu de la VM ne change pas.

## Validation

Les tests natifs couvrent l’option désactivée, les deux configurations de moteur,
la création et modification de missions planifiées, les lectures, l’annulation,
le refus de délégation plus large, les écritures invalides atomiques, les anciens
grants et clients, la révocation et la liaison du canal à une seule exécution.
Les parcours web et Android vérifient la persistance du choix dans les éditeurs.
