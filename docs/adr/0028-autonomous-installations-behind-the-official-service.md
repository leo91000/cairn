# Installations autonomes, accessibles uniquement par le service officiel

Date : 2026-10-03.

Leo devient un service officiel hébergé (interface web, application Android, API et comptes Leo) auquel chacun rattache ses propres installations. Une installation reste un ensemble autonome déployé par une commande `curl` : elle conserve les conversations, les projets, les comptes d’agent de code, les secrets et les disques. Le service officiel ne conserve que les comptes Leo, les rattachements, les membres et les rôles ; il regroupe toutes les installations d’un compte dans une même interface.

L’autre option était une plateforme centrale où le service officiel deviendrait le manager de tout le monde, les personnes n’apportant que des nodes d’exécution. Elle aurait rendu le service officiel responsable des abonnements ChatGPT et Claude, des jetons GitHub et 1Password et du stockage S3 de tous les utilisateurs, et aurait contredit le modèle de confiance des nodes, qui appartiennent au propriétaire de leur manager. Une connexion directe du navigateur à chaque installation a aussi été écartée : elle impose un domaine, un certificat et un port entrant, incompatibles avec une installation en une commande.

Conséquences difficiles à inverser :

- L’installation ouvre elle-même une connexion sortante vers le service officiel, qui relaie l’API et les flux de l’interface. Aucun port entrant ni URL publique n’est requis. Le relais voit passer le contenu des conversations sans le conserver ; il n’y a pas de chiffrement de bout en bout, puisque le service officiel sert déjà le code de l’interface.
- Le compte Leo est la seule manière d’accéder à une installation. Le mot de passe administrateur local et `SETUP_TOKEN` disparaissent ; une panne du service officiel rend les installations inaccessibles depuis l’interface, sans interrompre les exécutions en cours.
- Le service officiel fait autorité sur le propriétaire, les membres et leurs rôles. L’installation accepte les requêtes de son tunnel authentifié avec l’identité et le rôle indiqués par le relais, et ne tient pas sa propre liste.
- La revendication se fait par une commande pré-remplie avec un code à usage unique, ou en secours par `leo claim`, qui affiche un code à valider dans l’application. Une installation non revendiquée n’est accessible à personne. Les installations existantes ne sont pas migrées : elles sont revendiquées ou réinstallées.
- Les clients MCP externes, les liens publics des livrables et les notifications push passent par le service officiel. Un lien public ne fonctionne que lorsque son installation est en ligne.
- Une installation créée par `curl` occupe une seule machine avec un stockage S3 local intégré, ce qui préserve l’ADR-0009 ; un S3 externe peut le remplacer. Des nodes d’exécution supplémentaires doivent joindre le manager directement (réseau local, VPN ou URL publique facultative) : les disques ne transitent pas par le relais.
- Le service officiel fixe la version approuvée des installations, qui se mettent à jour automatiquement avec le mécanisme de vidage et de retour arrière des nodes. Le relais refuse une installation dont le protocole est incompatible et l’interface l’affiche comme à mettre à jour ; l’interface ne prend en charge qu’une version d’API, à une version près pendant un déploiement.
- Si le propriétaire supprime son compte Leo ou détache l’installation, elle redevient non revendiquée et inaccessible, ses données conservées ; les membres perdent l’accès. Le transfert de propriété n’est pas prévu.
- Le service officiel vit dans ce dépôt, comme un binaire distinct avec sa propre base Postgres, et partage avec l’installation les types du protocole de relais. Les installations gardent SQLite.

- L’approbation est la réponse de l’origine HTTPS officielle configurée, sans redirection, avec une image immuable vérifiée par digest Docker. Elle ne porte pas de signature distincte ni de compteur anti-rétrogradation ; l’opérateur peut réapprouver une image antérieure compatible pour un retour arrière. Les superviseurs hôtes sont rafraîchis en relançant la commande officielle d’installation.
