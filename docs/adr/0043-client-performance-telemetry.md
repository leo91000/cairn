# Diagnostics de performances des clients envoyés à Beacon par défaut

Décision de Léo, 2026-10-15 : les lenteurs de l’application doivent être observables
sans demander à chaque personne de copier manuellement un rapport. L’envoi des
mesures Android à Beacon est activé par défaut et désactivable dans les paramètres
du téléphone. Le panneau explique les données envoyées et leur conservation.

Cette décision étend les métadonnées conservées par Beacon dans l’ADR-0028 : elle
ajoute des mesures techniques du client, sans modifier la conservation du travail
des agents sur les installations. Les rapports sont associés au compte Cairn
authentifié ; ils ne sont pas anonymes. Ils ne contiennent ni conversations,
identifiants de conversation ou d’installation, fichiers, commandes, URLs,
adresses e-mail, cookies, jetons, exceptions ou traces de pile.

Le client envoie des compteurs et durées agrégés par catégorie d’écran et d’opération,
sa version, le modèle du téléphone, la version Android, la mémoire Java et la route
directe/relais courante. Un identifiant aléatoire désigne une fenêtre de mesure,
sans identifiant matériel ou d’installation de l’app. Il change au redémarrage du
processus, au changement de compte et à la remise à zéro locale.

L’envoi ne fonctionne qu’au premier plan et après connexion au compte, même sans
installation sélectionnée. Il commence après dix secondes et se répète au plus une
fois par minute. Désactiver le réglage, se déconnecter ou mettre l’application en
arrière-plan annule les envois. Les erreurs restent silencieuses ; les requêtes sont
limitées à cinq secondes, sans file persistante ni reprise en arrière-plan.

Beacon valide un schéma fermé et impose une taille de 64 Kio, une limite de six
envois par minute et par compte, au plus 256 fenêtres par compte et une conservation
de 14 jours. Chaque envoi remplace la dernière révision d’une même fenêtre ; les
doublons et les révisions anciennes ne multiplient pas les compteurs. La suppression
du compte supprime ses rapports. La désactivation arrête les envois futurs ; les
rapports déjà reçus suivent leur délai de conservation.

L’API de consultation d’un compte ne montre que ses propres rapports. L’opérateur
analyse les mesures par version et appareil via la base Beacon, avec ses accès
d’exploitation existants. Aucun accès global aux diagnostics n’est donné aux
comptes de l’application.

La consultation ajoute à chaque rapport `receivedAt`, la date serveur de sa
dernière révision acceptée, en millisecondes Unix. Elle provient du `updated_at`
déjà conservé par Beacon, jamais de l’horloge du client. Un doublon ou une
ancienne révision ne la rafraîchit pas. Ce champ est une métadonnée de lecture ;
il ne fait pas partie des schémas fermés envoyés par les clients et ne change ni
la cadence ni la conservation. Les mesures restent cumulées dans leur fenêtre :
une réception récente ne rend pas récents ses anciens maxima. Comparer les
révisions d’une même fenêtre pour mesurer un intervalle.

Extension du 2026-10-30 : le navigateur suit la même règle d’envoi par défaut,
désactivable dans les paramètres du compte, même sans installation. Le réglage web
est partagé entre les sous-domaines de Beacon ; le réglage Android reste propre au
téléphone. Les diagnostics relèvent du compte personnel, hors des droits MCP et
`cairn_installation` d’une installation. Android conserve son schéma 1 et ses mesures
de frames natives. Le schéma 2 fermé du web ajoute la famille de navigateur, une
empreinte SHA-256 des sources et dépendances du frontend construit, les appels API
terminés ou encore en cours, leurs erreurs/annulations, le décodage JSON et le
rendu Markdown. L’empreinte identifie les entrées du build servi, pas le commit
du serveur ni une attestation binaire ; elle inclut la version/plateforme de Node.
Elle ne dépend ni de Git ni d’un identifiant propre à la personne.

Le web mesure le délai de la boucle d’événements après des interactions, une
navigation ou une réponse API, par sondes ponctuelles regroupées à au plus deux
par seconde, sans réveil périodique au repos. Les navigateurs compatibles ajoutent
les durées des Long Tasks, sans lire leur attribution. L’absence de cette API est
explicite dans le rapport. Ces mesures ne sont pas des frames GPU et les durées API
incluent les attentes réseau et la récupération de connexion ; les appels non
terminés restent visibles séparément. Elles ne mesurent pas le temps de calcul du
serveur. Les catégories d’écran proviennent des modèles de routes et ne conservent
jamais leurs paramètres. Les fenêtres et observations sont annulées à la remise à
zéro et au changement de compte ; les appels qui traversent une période masquée
ne sont pas attribués à la nouvelle période au premier plan.

Les agrégats restent en mémoire : au plus 80 couples opération/écran, 64 appels en
cours suivis, 24 catégories de sonde, huit catégories Long Tasks et 40 échantillons
lents. Aucun rapport n’est stocké dans les cookies ou le stockage local ; seul le
choix booléen d’envoi y est conservé. Les envois web utilisent le même endpoint
authentifié et les mêmes limites globales par compte qu’Android. L’envoi est direct
à Beacon, sans instrumenter sa propre requête, sans `sendBeacon` ni `keepalive`.
Une erreur, une bascule du réglage ou un changement de compte ne réinitialise pas
la cadence maximale d’un essai par minute. Le panneau permet de rafraîchir,
réinitialiser et copier le rapport local, ainsi que de le partager quand l’API
native du navigateur est disponible.
