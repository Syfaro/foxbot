-botName = @FoxBot
-creatorName = @Syfaro

# General Onboarding
welcome =
    Hi, I'm { -botName }.
    
    If you use me as an inline bot, I'll mirror content from many furry sites. When I post the image I'll include a direct link and a source link, if available.
    
    If you send me an image, I'll try to find the source on FA.
    
    Contact my creator { -creatorName } if you have any issues or feature suggestions.

# Inline Help Text
inline-help-inline-title = Type your link or click me for more info
inline-help-inline-body =
    Hi there! I'm { -botName }.
    
    By typing my name into the Telegram message box followed by a link, I'll grab your image and let you send it to your chats while adding a source and direct image link. You can even add your Twitter account to get content from locked accounts.
    
    If you send me a photo directly, I'll try and find its source on FurAffinity.

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

# Error Messages
error-generic = Something went wrong, please try again later.
