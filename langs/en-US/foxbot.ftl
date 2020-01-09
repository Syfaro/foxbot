-botName = @FoxBot
-creatorName = @Syfaro

# General Onboarding
welcome =
    Hi, I'm { -botName }.
    
    If you use me as an inline bot, I'll mirror content from many furry sites. When I post the image I'll include a direct link and a source link, if available. You can add your Twitter account with /twitter to get content from locked accounts you follow.
    
    If you send me an image, I'll try to find the source on FA.
    
    Add me to your group for features like /mirror (where I mirror all the links in a message, including messages you reply to) or /source (where I attempt to find the source of an image you're replying to).
    
    Contact my creator { -creatorName } if you have any issues or feature suggestions.

# Inline Keyboard
inline-direct = Direct Link
inline-source = Source

# No Inline Results
inline-no-results-title = No results found
inline-no-results-body = I could not find any results for the provided query.

# Reverse Search
reverse-no-results = I was unable to find anything, sorry.
reverse-good-result = I found this (distance of { $distance }): { $link }
reverse-bad-result = I found this but it may not be the same image, be warned (distance of { $distance }): { $link }

# Twitter Onboarding
twitter-oob = Please follow the link and enter the 6 digit code returned: { $link }
twitter-welcome = Welcome aboard, { $userName }!
twitter-private = Let's do this in a private chat.

# In-group mirroring
mirror-no-links = Sorry, I could not find any links to mirror images from.
mirror-no-results = Sorry, I could not get any images from this message.
mirror-missing =
    In addition to these images, I could not fetch images from the following links:
    
    { $links }
    
    Sorry about that.

# In-group sourcing
source-no-photo = Sorry, I can't seem to find the photo here.

# Error Messages
error-generic = Oh no, something went wrong! Please send a message to my creator { -creatorName } if you continue having issues.
error-uuid = Oh no, something went wrong! Please send a message to my creator { -creatorName } with this ID if you continue having issues: { $uuid }
