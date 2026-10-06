# Un seul processus officiel de relais avant coordination distribuée

Date : 2026-10-06. Complément de l’ADR-0028 et restriction explicite de la
spec #43 (« plusieurs processus possibles ») pour la première livraison de #59.

Le déploiement supporté utilise un seul processus officiel pour les mutations de
compte et d’accès, les requêtes d’installation et les WebSockets du relais. Les
connexions et leurs notifications de révocation vivent dans ce processus. Cette
restriction permet de couper immédiatement les flux concernés à la déconnexion,
au retrait, au détachement, à la rotation ou à la révocation définitive d’une
installation. La vérification persistée ne remplace pas cette garantie locale.

Des réplicas avec affinité par installation ont été écartés : une déconnexion
sur un processus ne notifierait pas les flux de cette session sur les autres.
Une coordination distribuée demanderait un routage commun des connexions et des
notifications de révocation avec récupération des notifications manquées. Cette
mise en œuvre est différée ; ajouter des réplicas exige de réviser cette décision
et de tester les révocations entre processus, y compris après reconnexion.

Pour les modifications externes à ce processus (par exemple une suppression
opérateur en base), un tunnel établi vérifie son identité toutes les 30 secondes,
avec un délai maximal de cinq secondes par vérification. Une révocation confirmée
ferme le tunnel ; une erreur temporaire ne le ferme qu’après trois échecs
consécutifs, avec remise à zéro après une réussite. La fermeture peut donc prendre
environ 95 secondes après la dernière vérification réussie si chaque requête
expire. Ce compromis réduit les lectures Postgres par tunnel et évite qu’une
panne brève coupe toutes les installations. L’ouverture d’un tunnel reste refusée
en cas d’erreur. Les expirations de session et les annulations de flux sont
traitées indépendamment des attentes en base. Les exécutions déjà admises sur
l’installation continuent pendant une coupure du relais.
