# Blocs S3 partagés et objets immuables

Les états publiés partagent les blocs identiques dans un même stockage du manager. SQLite conserve les références durables des publications, y compris celles en cours, et les manifestes authentifiés portent les identifiants nécessaires aux lectures sans consulter cet inventaire. Chaque nouvelle copie possède un identifiant d'objet distinct : une suppression S3 retardée ne peut donc jamais effacer une copie de remplacement du même contenu. La collecte distante s'exécute séparément de la synchronisation, après retrait des références et expiration d'un délai de cinq minutes.

Les disques indépendants restent synchronisés en parallèle. Si deux premières publications envoient le même bloc simultanément, une transaction fait converger leurs références vers le premier objet vérifié et rend l'autre copie collectable avant publication des manifestes.

Les publications existantes restent lisibles et passent au format partagé lors de leur prochaine capture. La version de base de données passe à 5 pour empêcher un ancien exécutable d'ouvrir ces références. Le retour à une ancienne version exige la restauration cohérente de la base et des objets correspondants.
