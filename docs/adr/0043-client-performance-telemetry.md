# Diagnostics de performances Android envoyés à Beacon par défaut

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
