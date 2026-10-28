> Statut : précisée par [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) : avoir accès à un agent qui a l’accès à l’installation revient à avoir accès à tous les agents ; un tel agent ne peut pas donner à un compte l’accès à un agent, et il n’écrit des règles que pour l’auteur de la dernière intervention humaine de son tour, et aucune dans un tour sans intervention humaine ; il ne met pas en pause ni ne supprime la règle d’un autre compte et ne lève pas la pause d’urgence du propriétaire.

# Accès explicite d’un agent à l’installation courante

Date : 2026-10-11.

Mise à jour : 2026-10-14 — l’accès porte sur tout le catalogue MCP de gestion.

## Constat

Les agents ont un canal MCP privé (ADR-0035), mais consulter les missions et les
exécutions exigeait de configurer Cairn comme connexion externe, avec un jeton
personnel ou OAuth. Ce détour ne représentait pas les permissions de l’agent.

## Décision

Le propriétaire peut cocher l’accès à l’installation courante dans l’éditeur
d’un agent, sur le web et Android. `access.installation` vaut `false` par défaut,
y compris pour Main. Depuis #403, les profils utilisent un remplacement complet
avec révision attendue ; aucun chemin de mise à jour partielle ou ancien format
n’est conservé.

L’option ajoute `cairn_installation` aux configurations Codex et Claude Code.
`/mcp-installation` réutilise le canal privé et le jeton de l’exécution. Le droit
est enregistré dans le grant : connaître l’URL, ou cocher l’option après émission,
ne confère aucun accès. Le manager vérifie l’exécution active, le tour courant,
l’absence d’annulation et les permissions actuelles avant chaque requête.
Le canal refuse le jeton d’une autre exécution, sur la node locale comme distante.

Le catalogue reprend tous les contrats publics MCP de gestion : agents, projets,
skills, connexions MCP, missions et exécutions. La première version limitait cet
accès aux missions et exécutions de l’agent appelant ; cette limite empêchait le
propriétaire de lui demander de créer une skill globale dans Cairn.

Cocher cette option délègue désormais la gestion de toute l’installation, même
si les accès directs de l’agent aux projets, skills, connexions, GitHub ou nodes
sont restreints. Il peut éditer les profils et leur confier des missions ; ces
missions utilisent leurs propres identifiants et grants. L’arbitrage du propriétaire
pour #395/#403 interdit néanmoins à un agent d’élargir les droits d’un profil ou de
lui attribuer un plugin, une node exclusive ou l’exécution native. Ces actions
restent réservées au propriétaire ; aucun flux de confirmation ponctuelle côté
agent n’est construit dans cette tranche. Cette autorité est celle déjà accordée au MCP externe de gestion, plutôt
qu’un nouveau sous-ensemble divergent de ses outils. Elle n’ajoute pas de fonctions
d’administration absentes du catalogue MCP public, notamment pour les comptes.

Chaque requête revérifie le grant de l’exécution et les permissions actuelles.
Toutes les écritures de gestion revérifient le grant dans leur transaction,
après la préparation asynchrone et les attentes de verrou. Les skills préparent
leur fichier temporaire avant cette vérification, puis publient le fichier final
sous la même transaction d’autorisation. La découverte MCP revérifie aussi le
grant avant la requête externe et avant d’enregistrer son résultat. Les écritures
OAuth de découverte, d’enregistrement du client et de renouvellement des jetons
revérifient ce même grant dans la transaction de publication.
La création d’une mission utilise l’agent appelant par défaut, mais un autre agent
peut être choisi explicitement. Les listes et lectures couvrent l’installation
entière. Les attentes durables peuvent aussi viser les exécutions d’autres agents.

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
les écritures invalides atomiques, les anciens
grants et clients, la révocation et la liaison du canal à une seule exécution.
Les parcours web et Android vérifient la persistance du choix dans les éditeurs.
Les tests couvrent aussi la parité avec le catalogue MCP public, la création et
modification de ressources (dont les skills globales et de projet), et la gestion
des missions et exécutions d’autres agents, ainsi que la révocation pendant
une découverte MCP ou un renouvellement OAuth en cours.
